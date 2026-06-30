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
use usbforge_core::iso::{self, IsoReader};
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
    /// Create a (UEFI-)bootable USB from an ISO by file-copy. DESTROYS all data.
    Create {
        /// Source ISO image.
        iso: String,
        /// Target device path or id (e.g. /dev/sdb or sdb). See `list --all`.
        device: String,
        /// Partition scheme.
        #[arg(long, value_enum, default_value = "gpt")]
        scheme: SchemeArg,
        /// Filesystem: auto (NTFS for Windows ISOs, else FAT32), fat32, or ntfs.
        #[arg(long, value_enum, default_value = "auto")]
        fs: CreateFs,
        /// Volume label (default: the ISO's label, else USBFORGE).
        #[arg(long)]
        label: Option<String>,
        /// Skip the interactive confirmation prompt (required for scripts).
        #[arg(long)]
        yes: bool,
        /// Permit writing to a non-removable (fixed/internal) disk.
        #[arg(long)]
        allow_fixed: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum CreateFs {
    Auto,
    Fat32,
    Ntfs,
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
        Command::Create {
            iso,
            device,
            scheme,
            fs,
            label,
            yes,
            allow_fixed,
        } => cmd_create(&iso, &device, scheme, fs, label, yes, allow_fixed),
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

    if matches!(info.kind, ImageKind::Iso) {
        match IsoReader::open(path) {
            Ok(reader) => {
                let r = reader.report();
                if !r.volume_label.is_empty() {
                    println!("  volume label: {}", r.volume_label);
                }
                println!(
                    "  contents: {} files, {}",
                    r.total_files,
                    humanize_bytes(r.total_bytes)
                );
                println!(
                    "  UEFI boot: {}",
                    if r.is_uefi_bootable() {
                        r.uefi_archs.join(", ")
                    } else {
                        "no".to_string()
                    }
                );
                if r.windows_installer {
                    println!("  Windows installer: yes");
                }
                if let Some(b) = &r.bios_bootloader {
                    println!("  BIOS bootloader: {b}");
                }
                println!(
                    "  isohybrid (raw write boots BIOS+UEFI): {}",
                    if r.isohybrid { "yes" } else { "no" }
                );
            }
            Err(e) => println!("  (ISO9660 parse failed: {e})"),
        }
    }
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
    if matches!(info.kind, ImageKind::Iso) && iso::is_isohybrid(image) {
        eprintln!("Note: isohybrid ISO — the resulting drive will boot on both BIOS and UEFI.");
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

    let region = layout::write_single_partition(&mut *target, scheme, fs_kind, &label, false)?;
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

fn cmd_create(
    iso_path: &str,
    device_arg: &str,
    scheme: SchemeArg,
    fs: CreateFs,
    label: Option<String>,
    yes: bool,
    allow_fixed: bool,
) -> Result<()> {
    let reader = IsoReader::open(iso_path).context("failed to open ISO")?;
    let report = reader.report();
    let scheme: PartitionScheme = scheme.into();

    eprintln!("\nSource ISO: {iso_path}");
    eprintln!(
        "  label: {}",
        if report.volume_label.is_empty() {
            "(none)"
        } else {
            &report.volume_label
        }
    );
    eprintln!(
        "  contents: {} files, {}",
        report.total_files,
        humanize_bytes(report.total_bytes)
    );
    eprintln!(
        "  UEFI bootable: {}",
        if report.is_uefi_bootable() {
            report.uefi_archs.join(", ")
        } else {
            "no (no /EFI/BOOT/BOOT*.EFI found)".to_string()
        }
    );
    if report.windows_installer {
        eprintln!("  Windows installer detected");
    }
    if let Some(b) = &report.bios_bootloader {
        eprintln!("  BIOS bootloader: {b}");
    }
    if report.isohybrid {
        eprintln!(
            "  isohybrid: yes — for BIOS-machine boot, use `usbforge write {iso_path} {device_arg}`\n\
             (raw mode boots BIOS+UEFI). The file-copy create below is UEFI-boot only."
        );
    } else if report.bios_bootloader.is_some() {
        eprintln!("  (BIOS-only boot for non-isohybrid ISOs needs a syslinux/GRUB install — not yet implemented; UEFI works.)");
    }

    // Label: explicit flag > ISO volume label > default.
    let label = label
        .filter(|s| !s.is_empty())
        .or_else(|| (!report.volume_label.is_empty()).then(|| report.volume_label.clone()))
        .unwrap_or_else(|| "USBFORGE".to_string());

    // NTFS (UEFI:NTFS) for Windows ISOs / big files; FAT32 otherwise.
    let use_ntfs = match fs {
        CreateFs::Ntfs => true,
        CreateFs::Fat32 => false,
        CreateFs::Auto => report.windows_installer,
    };
    if use_ntfs && !tool_exists("mkfs.ntfs") {
        bail!("NTFS mode needs `mkfs.ntfs` — install ntfs-3g + ntfsprogs");
    }

    let device = resolve_target(device_arg, allow_fixed)?;
    if report.total_bytes > device.size {
        bail!(
            "ISO contents ({}) don't fit on device {} ({})",
            humanize_bytes(report.total_bytes),
            device.path,
            device.size_human()
        );
    }

    eprintln!("\nAbout to CREATE bootable media:");
    eprintln!(
        "  target: {}  [{}]  {}  {}",
        device.path,
        device.bus,
        device.size_human(),
        device.display_name()
    );
    eprintln!(
        "  scheme: {scheme:?}    fs: {}    label: {label}",
        if use_ntfs {
            "NTFS (UEFI:NTFS)"
        } else {
            "FAT32"
        }
    );
    eprintln!(
        "\n  !! ALL DATA ON {} WILL BE PERMANENTLY DESTROYED. !!",
        device.path
    );
    confirm_destruction(yes, &device.path)?;

    let reporter = CliReporter::new();
    let summary = if use_ntfs {
        create_uefi_ntfs(&device, scheme, &reader, &label, &reporter)?
    } else {
        let mut target = usbforge_platform::disk_access()
            .open(&device, Access::ReadWriteExclusive)
            .context("failed to open device (need elevated privileges?)")?;
        let stats = reader.install_to_device(&mut *target, scheme, &label, &reporter)?;
        target.sync()?;
        format!(
            "copied {} files ({})",
            stats.files,
            humanize_bytes(stats.bytes)
        )
    };
    eprintln!();

    println!(
        "Done — {summary}. UEFI-bootable: {}.",
        if report.is_uefi_bootable() {
            "yes"
        } else {
            "no (ISO has no UEFI boot files)"
        }
    );
    Ok(())
}

/// UEFI:NTFS create (Linux, host-tool assisted): write the two-partition layout
/// (NTFS main plus a tiny FAT ESP holding the UEFI:NTFS bootloader), run
/// `mkfs.ntfs` on the main partition, mount it, copy the ISO tree in, unmount.
fn create_uefi_ntfs(
    device: &Device,
    scheme: PartitionScheme,
    reader: &IsoReader,
    label: &str,
    reporter: &CliReporter,
) -> Result<String> {
    use usbforge_core::{layout, uefi_ntfs};

    // 1) Partition table + bootloader, written through the whole-disk handle.
    {
        let mut target = usbforge_platform::disk_access()
            .open(device, Access::ReadWriteExclusive)
            .context("failed to open device (need elevated privileges?)")?;
        let (main, esp) = layout::write_uefi_ntfs_layout(&mut *target, scheme)?;
        eprintln!(
            "Layout: NTFS main {} + UEFI:NTFS ESP {} at offset {}.",
            humanize_bytes(main.len),
            humanize_bytes(esp.len),
            esp.start
        );
        uefi_ntfs::write_esp(&mut *target, esp.start)?;
        target.sync()?;
        // handle dropped here → releases O_EXCL so the kernel can re-read
    }

    // 2) Make the kernel pick up the new partition nodes.
    let _ = std::process::Command::new("partprobe")
        .arg(&device.path)
        .status();
    let _ = std::process::Command::new("udevadm").arg("settle").status();

    let part = partition_path(&device.path, 1);
    let node = std::path::Path::new(&part);
    for _ in 0..50 {
        if node.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !node.exists() {
        bail!("partition node {part} did not appear after re-reading the table");
    }

    // 3) Format the main partition NTFS.
    eprintln!("Formatting {part} as NTFS …");
    run_tool("mkfs.ntfs", &["-Q", "-F", "-L", label, &part]).context("mkfs.ntfs failed")?;

    // 4) Mount, copy the ISO tree in, unmount.
    let mnt = std::env::temp_dir().join(format!("usbforge_ntfs_{}", std::process::id()));
    std::fs::create_dir_all(&mnt)?;
    let mnt_str = mnt.to_string_lossy().to_string();
    run_tool("mount", &["-t", "ntfs-3g", &part, &mnt_str]).context("mounting NTFS failed")?;

    let extract = reader.extract_to_dir(&mnt, reporter);

    let _ = std::process::Command::new("sync").status();
    let _ = std::process::Command::new("umount").arg(&mnt_str).status();
    let _ = std::fs::remove_dir_all(&mnt);

    let stats = extract?;
    Ok(format!(
        "copied {} files ({}) to NTFS + wrote UEFI:NTFS bootloader",
        stats.files,
        humanize_bytes(stats.bytes)
    ))
}

/// Is an executable of this name on PATH?
fn tool_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(name).is_file()))
        .unwrap_or(false)
}

/// `/dev/sdb` + 1 → `/dev/sdb1`; `/dev/nvme0n1` + 1 → `/dev/nvme0n1p1`.
fn partition_path(disk: &str, n: u32) -> String {
    let needs_p = disk.chars().last().is_some_and(|c| c.is_ascii_digit());
    if needs_p {
        format!("{disk}p{n}")
    } else {
        format!("{disk}{n}")
    }
}

/// Run a host tool and fail if it returns non-zero.
fn run_tool(cmd: &str, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new(cmd)
        .args(args)
        .status()
        .with_context(|| format!("could not run `{cmd}`"))?;
    if !status.success() {
        bail!("`{cmd}` failed (exit {:?})", status.code());
    }
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
