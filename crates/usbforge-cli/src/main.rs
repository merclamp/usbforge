//! USBForge command-line frontend.
//!
//! A thin shell over `usbforge-core` + `usbforge-platform`. It doubles as the
//! proof-of-concept for the portable core: `list` exercises device enumeration,
//! `hash`/`inspect` exercise the image + hashing modules — all without any GUI.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write as _};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};

use usbforge_core::device::{humanize_bytes, Device};
use usbforge_core::disk::Access;
use usbforge_core::filesystem::{FileSystem, PartitionScheme};
use usbforge_core::format::{self, PartitionSlice};
use usbforge_core::hash::{self, Algo};
use usbforge_core::image::{ImageInfo, ImageKind};
use usbforge_core::layout;
use usbforge_core::report::{Level, Reporter};
use usbforge_core::write::{self, WriteOptions};
use usbforge_core::PRODUCT;

#[derive(Parser)]
#[command(
    name = "usbforge",
    version,
    about = "Cross-platform bootable USB / disk image writer"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List attached storage devices.
    List {
        /// Include fixed/internal disks (dangerous — off by default).
        #[arg(long)]
        all: bool,
    },
    /// Inspect a source image (size + detected kind).
    Inspect {
        /// Path to an .iso/.img/.vhd/... file.
        path: String,
    },
    /// Compute checksums of a file (image verification).
    Hash {
        /// Path to the file to hash.
        path: String,
        /// Algorithms: any of md5, sha1, sha256, sha512 (default: all).
        #[arg(long, value_delimiter = ',')]
        algo: Vec<String>,
    },
    /// Write a raw image (.iso/.img/.raw) to a device. DESTROYS all data on it.
    Write {
        /// Source image file.
        image: String,
        /// Target device path or id (e.g. /dev/sdb or sdb). See `list --all`.
        device: String,
        /// Skip the interactive confirmation prompt (required for scripts).
        #[arg(long)]
        yes: bool,
        /// Permit writing to a non-removable (fixed/internal) disk.
        #[arg(long)]
        allow_fixed: bool,
        /// Skip the read-back verification pass.
        #[arg(long)]
        no_verify: bool,
    },
    /// Partition + format a device (DESTROYS all data on it).
    Format {
        /// Target device path or id (e.g. /dev/sdb or sdb). See `list --all`.
        device: String,
        /// Partition scheme.
        #[arg(long, value_enum, default_value = "gpt")]
        scheme: SchemeArg,
        /// Filesystem to create.
        #[arg(long, value_enum, default_value = "fat32")]
        fs: FsArg,
        /// Volume label (default: USBFORGE).
        #[arg(long)]
        label: Option<String>,
        /// Skip the interactive confirmation prompt (required for scripts).
        #[arg(long)]
        yes: bool,
        /// Permit formatting a non-removable (fixed/internal) disk.
        #[arg(long)]
        allow_fixed: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum SchemeArg {
    Gpt,
    Mbr,
}

impl From<SchemeArg> for PartitionScheme {
    fn from(s: SchemeArg) -> Self {
        match s {
            SchemeArg::Gpt => PartitionScheme::Gpt,
            SchemeArg::Mbr => PartitionScheme::Mbr,
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum FsArg {
    Fat32,
}

impl From<FsArg> for FileSystem {
    fn from(f: FsArg) -> Self {
        match f {
            FsArg::Fat32 => FileSystem::Fat32,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::List { all } => cmd_list(all),
        Command::Inspect { path } => cmd_inspect(&path),
        Command::Hash { path, algo } => cmd_hash(&path, &algo),
        Command::Write {
            image,
            device,
            yes,
            allow_fixed,
            no_verify,
        } => cmd_write(&image, &device, yes, allow_fixed, no_verify),
        Command::Format {
            device,
            scheme,
            fs,
            label,
            yes,
            allow_fixed,
        } => cmd_format(&device, scheme, fs, label, yes, allow_fixed),
    }
}

fn cmd_list(all: bool) -> Result<()> {
    let enumerator = usbforge_platform::device_enumerator();
    let devices = enumerator
        .list(!all)
        .context("failed to enumerate devices")?;

    if devices.is_empty() {
        if all {
            println!("No storage devices found.");
        } else {
            println!("No removable devices found (use --all to include fixed disks).");
        }
        return Ok(());
    }

    print_device_table(&devices);
    if !all {
        println!("\n(showing removable media only; pass --all to include fixed disks)");
    }
    Ok(())
}

fn print_device_table(devices: &[Device]) {
    println!(
        "{:<14} {:>8} {:>8} {:<3} {:<3} NAME",
        "PATH", "BUS", "SIZE", "RM", "RO"
    );
    for d in devices {
        println!(
            "{:<14} {:>8} {:>8} {:<3} {:<3} {}",
            d.path,
            d.bus,
            d.size_human(),
            if d.removable { "yes" } else { "no" },
            if d.read_only { "yes" } else { "no" },
            d.display_name(),
        );
    }
}

fn cmd_inspect(path: &str) -> Result<()> {
    let info: ImageInfo = ImageInfo::inspect(path).context("failed to inspect image")?;
    println!("{PRODUCT} — image inspection");
    println!("  path: {}", info.path.display());
    println!(
        "  size: {} ({} bytes)",
        usbforge_core::device::humanize_bytes(info.size),
        info.size
    );
    println!("  kind: {}", info.kind.label());
    Ok(())
}

fn cmd_hash(path: &str, algo_args: &[String]) -> Result<()> {
    let algos = parse_algos(algo_args)?;
    let reporter = CliReporter::new();
    let digests = hash::hash_file(path, &algos, &reporter)?;
    // Progress prints carriage-return updates; finish the line.
    eprintln!();
    print_digests(&digests);
    Ok(())
}

fn parse_algos(args: &[String]) -> Result<Vec<Algo>> {
    if args.is_empty() {
        return Ok(Algo::all().to_vec());
    }
    let mut algos = Vec::new();
    for a in args {
        let parsed = match a.to_ascii_lowercase().replace('-', "").as_str() {
            "md5" => Algo::Md5,
            "sha1" => Algo::Sha1,
            "sha256" => Algo::Sha256,
            "sha512" => Algo::Sha512,
            other => anyhow::bail!("unknown algorithm: {other}"),
        };
        algos.push(parsed);
    }
    Ok(algos)
}

fn print_digests(digests: &BTreeMap<Algo, String>) {
    for (algo, digest) in digests {
        println!("{:<8} {}", algo.name(), digest);
    }
}

fn cmd_write(
    image: &str,
    device_arg: &str,
    yes: bool,
    allow_fixed: bool,
    no_verify: bool,
) -> Result<()> {
    let info = ImageInfo::inspect(image).context("failed to inspect image")?;
    if matches!(info.kind, ImageKind::CompressedDisk) {
        bail!("compressed images are not supported yet (planned for M3) — decompress it first");
    }

    let device = resolve_target(device_arg, allow_fixed)?;
    if info.size > device.size {
        bail!(
            "image ({}) is larger than device {} ({})",
            humanize_bytes(info.size),
            device.path,
            device.size_human()
        );
    }

    eprintln!("\nAbout to write:");
    eprintln!(
        "  image:  {image}  ({}, {})",
        info.kind.label(),
        humanize_bytes(info.size)
    );
    eprintln!(
        "  target: {}  [{}]  {}  {}",
        device.path,
        device.bus,
        device.size_human(),
        device.display_name()
    );
    eprintln!(
        "\n  !! ALL DATA ON {} WILL BE PERMANENTLY DESTROYED. !!",
        device.path
    );
    confirm_destruction(yes, &device.path)?;

    // ---- write ------------------------------------------------------------
    let mut target = usbforge_platform::disk_access()
        .open(&device, Access::ReadWriteExclusive)
        .context("failed to open device for writing (need elevated privileges?)")?;

    let reporter = CliReporter::new();
    let summary = write::write_image_file(
        Path::new(image),
        &mut *target,
        &WriteOptions {
            verify: !no_verify,
            ..Default::default()
        },
        &reporter,
    )?;
    eprintln!();

    println!(
        "Done — wrote {}{}.",
        humanize_bytes(summary.bytes_written),
        if summary.verified { " (verified)" } else { "" }
    );
    Ok(())
}

/// Resolve a device by path/id and apply the "is this safe to clobber?" guards.
/// Returns an owned [`Device`] so callers hold no borrow on the device list.
fn resolve_target(device_arg: &str, allow_fixed: bool) -> Result<Device> {
    let device = usbforge_platform::device_enumerator()
        .list(false)
        .context("failed to enumerate devices")?
        .into_iter()
        .find(|d| d.path == device_arg || d.id == device_arg)
        .ok_or_else(|| {
            anyhow!("device '{device_arg}' not found; run `usbforge list --all` to see ids/paths")
        })?;

    if device.read_only {
        bail!("{} is write-protected", device.path);
    }
    if !device.is_removable_media() && !allow_fixed {
        bail!(
            "{} looks like a fixed/internal disk ({}) — refusing.\n\
             Re-run with --allow-fixed only if you are absolutely certain.",
            device.path,
            device.display_name()
        );
    }
    Ok(device)
}

/// Typed-path confirmation gate for a destructive operation.
fn confirm_destruction(yes: bool, device_path: &str) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!("refusing without confirmation (re-run with --yes for non-interactive use)");
    }
    print!("\nType the device path ({device_path}) to confirm: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    if line.trim() != device_path {
        bail!("confirmation did not match; aborted");
    }
    Ok(())
}

fn cmd_format(
    device_arg: &str,
    scheme: SchemeArg,
    fs: FsArg,
    label: Option<String>,
    yes: bool,
    allow_fixed: bool,
) -> Result<()> {
    let device = resolve_target(device_arg, allow_fixed)?;
    let scheme: PartitionScheme = scheme.into();
    let fs_kind: FileSystem = fs.into();
    let label = label.unwrap_or_else(|| "USBFORGE".to_string());

    eprintln!("\nAbout to PARTITION + FORMAT:");
    eprintln!(
        "  target: {}  [{}]  {}  {}",
        device.path,
        device.bus,
        device.size_human(),
        device.display_name()
    );
    eprintln!("  scheme: {scheme:?}    fs: {fs_kind}    label: {label}");
    eprintln!(
        "\n  !! ALL DATA ON {} WILL BE PERMANENTLY DESTROYED. !!",
        device.path
    );
    confirm_destruction(yes, &device.path)?;

    let mut target = usbforge_platform::disk_access()
        .open(&device, Access::ReadWriteExclusive)
        .context("failed to open device (need elevated privileges?)")?;

    let region = layout::write_single_partition(&mut *target, scheme, fs_kind, &label)?;
    eprintln!(
        "Partition table written ({scheme:?}); data partition at offset {} ({}).",
        region.start,
        humanize_bytes(region.len)
    );

    match fs_kind {
        FileSystem::Fat32 => {
            let mut slice = PartitionSlice::new(&mut *target, region.start, region.len);
            format::format_fat32(&mut slice, &label)?;
        }
        other => bail!(
            "formatting {other} is not implemented yet (M2 covers FAT32; exFAT/ext4/NTFS next)"
        ),
    }

    target.sync()?;
    println!(
        "Done — {} formatted as {fs_kind} ({scheme:?}).",
        device.path
    );
    Ok(())
}

/// Minimal reporter: logs to stderr, progress as an in-place percentage.
struct CliReporter;

impl CliReporter {
    fn new() -> Self {
        CliReporter
    }
}

impl Reporter for CliReporter {
    fn log(&self, level: Level, message: &str) {
        eprintln!("[{level}] {message}");
    }
    fn progress(&self, operation: &str, fraction: f32) {
        eprint!(
            "\r{operation}: {:>3.0}%",
            (fraction * 100.0).clamp(0.0, 100.0)
        );
    }
}
