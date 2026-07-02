//! USBForge command-line frontend.
//!
//! A thin shell over `usbforge-core` + `usbforge-platform`. It doubles as the
//! proof-of-concept for the portable core: `list` exercises device enumeration,
//! `hash`/`inspect` exercise the image + hashing modules — all without any GUI.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read as _, Seek as _, SeekFrom, Write as _};
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
        /// Add an ext4 persistence partition for a live Linux USB (uses the
        /// remaining space after the boot partition).
        #[arg(long)]
        persistence: bool,
        /// Install syslinux for BIOS boot of a non-isohybrid ISO (requires
        /// --scheme mbr and the `syslinux` tool).
        #[arg(long)]
        bios: bool,
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
            persistence,
            bios,
            label,
            yes,
            allow_fixed,
        } => cmd_create(
            &iso,
            &device,
            scheme,
            fs,
            persistence,
            bios,
            label,
            yes,
            allow_fixed,
        ),
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
                if r.udf {
                    println!("  UDF: yes (may hold files > 4 GiB; use `create --fs ntfs`)");
                }
                if let Some(k) = r.persistence {
                    println!("  persistence: {} (use `create --persistence`)", k.label());
                }
            }
            Err(e) => {
                if iso::is_udf(path) {
                    println!("  UDF image — read it with `create --fs ntfs` (no ISO9660 view)");
                } else {
                    println!("  (ISO9660 parse failed: {e})");
                }
            }
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

#[allow(clippy::too_many_arguments)] // mirrors the clap `create` subcommand fields
fn cmd_create(
    iso_path: &str,
    device_arg: &str,
    scheme: SchemeArg,
    fs: CreateFs,
    persistence: bool,
    bios: bool,
    label: Option<String>,
    yes: bool,
    allow_fixed: bool,
) -> Result<()> {
    let scheme: PartitionScheme = scheme.into();
    let udf = iso::is_udf(iso_path);
    // Our ISO9660 reader parses ISO9660/Joliet; pure-UDF Windows ISOs won't
    // parse — fine, the NTFS path reads them through a kernel mount instead.
    let reader = IsoReader::open(iso_path).ok();
    let report = reader.as_ref().map(|r| r.report());

    eprintln!("\nSource ISO: {iso_path}");
    if let Some(r) = &report {
        eprintln!(
            "  label: {}",
            if r.volume_label.is_empty() {
                "(none)"
            } else {
                &r.volume_label
            }
        );
        eprintln!(
            "  contents: {} files, {}",
            r.total_files,
            humanize_bytes(r.total_bytes)
        );
        eprintln!(
            "  UEFI bootable: {}",
            if r.is_uefi_bootable() {
                r.uefi_archs.join(", ")
            } else {
                "no (no /EFI/BOOT/BOOT*.EFI found)".to_string()
            }
        );
        if r.windows_installer {
            eprintln!("  Windows installer detected");
        }
        if let Some(b) = &r.bios_bootloader {
            eprintln!("  BIOS bootloader: {b}");
        }
        if r.isohybrid {
            eprintln!(
                "  isohybrid: yes — for BIOS-machine boot, use `usbforge write {iso_path} {device_arg}`\n\
                 (raw mode boots BIOS+UEFI). The file-copy create below is UEFI-boot only."
            );
        }
    } else if udf {
        eprintln!("  UDF image (no ISO9660 view) — will be read via a kernel mount.");
    } else {
        bail!("`{iso_path}` is not a readable ISO9660/UDF image");
    }
    if udf {
        eprintln!("  UDF: yes (supports files > 4 GiB)");
    }

    // Label: explicit flag > ISO volume label > default.
    let label = label
        .filter(|s| !s.is_empty())
        .or_else(|| {
            report
                .as_ref()
                .and_then(|r| (!r.volume_label.is_empty()).then(|| r.volume_label.clone()))
        })
        .unwrap_or_else(|| "USBFORGE".to_string());

    // NTFS (UEFI:NTFS) for Windows / UDF / >4 GiB; FAT32 otherwise.
    let use_ntfs = match fs {
        CreateFs::Ntfs => true,
        CreateFs::Fat32 => false,
        CreateFs::Auto => udf || report.as_ref().is_some_and(|r| r.windows_installer),
    };
    if use_ntfs && !tool_exists("mkfs.ntfs") {
        bail!("NTFS mode needs `mkfs.ntfs` — install ntfs-3g + ntfsprogs");
    }
    if !use_ntfs && reader.is_none() {
        bail!("this image needs NTFS mode (`--fs ntfs`); FAT32 file-copy can't read UDF");
    }
    if persistence {
        if use_ntfs {
            bail!("--persistence is for Linux live ISOs (FAT32 boot), not NTFS");
        }
        if reader.is_none() {
            bail!("--persistence needs a readable ISO9660 live image");
        }
        if !tool_exists("mkfs.ext4") {
            bail!("--persistence needs `mkfs.ext4` — install e2fsprogs");
        }
    }
    if bios {
        if use_ntfs || persistence {
            bail!("--bios can't be combined with NTFS or --persistence");
        }
        if scheme != PartitionScheme::Mbr {
            bail!("--bios requires --scheme mbr (BIOS chainloading)");
        }
        if reader.is_none() {
            bail!("--bios needs a readable ISO9660 image");
        }
        if !tool_exists("syslinux") {
            bail!("--bios needs the `syslinux` tool — install syslinux");
        }
    }

    let device = resolve_target(device_arg, allow_fixed)?;
    if let Some(r) = &report {
        if r.total_bytes > device.size {
            bail!(
                "ISO contents ({}) don't fit on device {} ({})",
                humanize_bytes(r.total_bytes),
                device.path,
                device.size_human()
            );
        }
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
    if persistence {
        eprintln!("  + ext4 persistence partition (uses the remaining space)");
    }
    if bios {
        eprintln!("  + syslinux BIOS boot (chainload MBR)");
    }
    eprintln!(
        "\n  !! ALL DATA ON {} WILL BE PERMANENTLY DESTROYED. !!",
        device.path
    );
    confirm_destruction(yes, &device.path)?;

    let reporter = CliReporter::new();
    let summary = if bios {
        let reader = reader.as_ref().expect("bios requires a readable ISO");
        create_bios_syslinux(&device, reader, &label, &reporter)?
    } else if persistence {
        let reader = reader
            .as_ref()
            .expect("persistence requires a readable ISO");
        create_persistence(&device, scheme, reader, report.as_ref(), &label, &reporter)?
    } else if use_ntfs {
        create_uefi_ntfs(&device, scheme, iso_path, &label, &reporter)?
    } else {
        let reader = reader.expect("FAT path requires a readable ISO9660 image");
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
        match report.as_ref().map(|r| r.is_uefi_bootable()) {
            Some(true) => "yes",
            Some(false) => "no (ISO has no UEFI boot files)",
            None => "yes (UDF / Windows install media)",
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
    iso_path: &str,
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

    // 4) Mount the NTFS target, copy the (mounted) ISO tree in, unmount.
    let mnt = std::env::temp_dir().join(format!("usbforge_ntfs_{}", std::process::id()));
    std::fs::create_dir_all(&mnt)?;
    let mnt_str = mnt.to_string_lossy().to_string();
    run_tool("mount", &["-t", "ntfs-3g", &part, &mnt_str]).context("mounting NTFS failed")?;

    let extract = mount_copy_iso(iso_path, &mnt, reporter);

    let _ = std::process::Command::new("sync").status();
    let _ = std::process::Command::new("umount").arg(&mnt_str).status();
    let _ = std::fs::remove_dir_all(&mnt);

    let (files, bytes) = extract?;
    Ok(format!(
        "copied {files} files ({}) to NTFS + wrote UEFI:NTFS bootloader",
        humanize_bytes(bytes)
    ))
}

/// BIOS boot via host syslinux (Linux): create an MBR FAT32 partition, extract
/// the ISO, install syslinux (its own tested ldlinux.sys patching) into the
/// isolinux directory, and write a chainloading MBR with partition 1 marked
/// active. The same FAT partition keeps the ISO's `/EFI/BOOT` files, so the
/// result boots on both BIOS and UEFI.
fn create_bios_syslinux(
    device: &Device,
    reader: &IsoReader,
    label: &str,
    reporter: &CliReporter,
) -> Result<String> {
    use usbforge_core::layout;

    let mbr_bin = Path::new("/usr/lib/syslinux/bios/mbr.bin");
    if !mbr_bin.exists() {
        bail!("syslinux MBR not found at {}", mbr_bin.display());
    }

    // 1) MBR FAT32 partition + ISO extract, through the whole-disk handle.
    let extract = {
        let mut target = usbforge_platform::disk_access()
            .open(device, Access::ReadWriteExclusive)
            .context("failed to open device (need elevated privileges?)")?;
        let region = layout::write_single_partition(
            &mut *target,
            PartitionScheme::Mbr,
            FileSystem::Fat32,
            label,
            false,
        )?;
        eprintln!(
            "Boot FAT32 partition created ({}).",
            humanize_bytes(region.len)
        );
        let stats = reader.install_to_region(&mut *target, region, label, None, reporter)?;
        target.sync()?;
        stats
    };

    // 2) Re-read the partition table.
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
        bail!("boot partition node {part} did not appear");
    }

    // 3) Find the isolinux/syslinux config dir and provide a syslinux.cfg.
    let cfg_dir = mount_prepare_syslinux_cfg(&part)?;

    // 4) Install syslinux into that dir (unmounted) — its installer patches ldlinux.sys.
    eprintln!("Installing syslinux into /{cfg_dir} …");
    run_tool(
        "syslinux",
        &["--directory", &format!("/{cfg_dir}/"), "--install", &part],
    )
    .context("syslinux install failed")?;

    // 5) Chainloading MBR + active flag.
    write_chainload_mbr(&device.path, mbr_bin)?;

    Ok(format!(
        "copied {} files ({}) + installed syslinux (BIOS) into /{cfg_dir} + chainload MBR",
        extract.files,
        humanize_bytes(extract.bytes)
    ))
}

/// Mount the boot FAT partition, locate the isolinux/syslinux config directory,
/// copy `isolinux.cfg` to `syslinux.cfg` so syslinux finds it, and return the
/// directory (relative path). Unmounts before returning.
fn mount_prepare_syslinux_cfg(part: &str) -> Result<String> {
    let mnt = std::env::temp_dir().join(format!("usbforge_syslinux_{}", std::process::id()));
    std::fs::create_dir_all(&mnt)?;
    let mnt_str = mnt.to_string_lossy().to_string();
    run_tool("mount", &[part, &mnt_str]).context("mounting boot partition failed")?;

    let result = (|| -> Result<String> {
        let candidates = ["isolinux", "boot/isolinux", "syslinux", "boot/syslinux"];
        let dir = candidates
            .iter()
            .copied()
            .find(|d| {
                mnt.join(d).join("isolinux.cfg").exists()
                    || mnt.join(d).join("syslinux.cfg").exists()
            })
            .ok_or_else(|| {
                anyhow!("no isolinux/syslinux config on the medium — is it a BIOS-bootable ISO?")
            })?
            .to_string();
        let iso_cfg = mnt.join(&dir).join("isolinux.cfg");
        let sys_cfg = mnt.join(&dir).join("syslinux.cfg");
        if iso_cfg.exists() && !sys_cfg.exists() {
            std::fs::copy(&iso_cfg, &sys_cfg).context("creating syslinux.cfg")?;
        }
        Ok(dir)
    })();

    let _ = std::process::Command::new("sync").status();
    let _ = std::process::Command::new("umount").arg(&mnt_str).status();
    let _ = std::fs::remove_dir_all(&mnt);
    result
}

/// Write syslinux's chainloading boot code into LBA0 (preserving the partition
/// table) and mark partition 1 active.
fn write_chainload_mbr(device_path: &str, mbr_bin: &Path) -> Result<()> {
    let code = std::fs::read(mbr_bin).context("reading syslinux mbr.bin")?;
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(device_path)
        .with_context(|| format!("opening {device_path}"))?;
    let mut lba0 = [0u8; 512];
    f.read_exact(&mut lba0)?;
    let n = code.len().min(440);
    lba0[..n].copy_from_slice(&code[..n]);
    lba0[446] = 0x80; // mark the first partition active
    lba0[510] = 0x55;
    lba0[511] = 0xAA;
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&lba0)?;
    f.sync_all()?;
    Ok(())
}

/// Persistence create (Linux, host-tool assisted): a boot FAT32 partition with
/// the live ISO, plus an ext4 partition (the rest of the device) labelled for
/// the distro's overlay (`casper-rw` / `persistence`).
fn create_persistence(
    device: &Device,
    scheme: PartitionScheme,
    reader: &IsoReader,
    report: Option<&usbforge_core::iso::IsoReport>,
    label: &str,
    reporter: &CliReporter,
) -> Result<String> {
    use usbforge_core::layout;

    let kind = report.and_then(|r| r.persistence);
    if kind.is_none() {
        eprintln!("  note: live-ISO family not detected; using `casper-rw` (Ubuntu) defaults.");
    }
    let persist_label = kind.map(|k| k.label()).unwrap_or("casper-rw");

    // Boot partition holds the ISO + headroom; the ext4 data partition gets the rest.
    let iso_bytes = report.map(|r| r.total_bytes).unwrap_or(0);
    let boot_bytes = (iso_bytes + 128 * 1024 * 1024).max(256 * 1024 * 1024);

    // 1) Two-partition layout + boot FAT32 + ISO extract, through the whole-disk handle.
    let extract = {
        let mut target = usbforge_platform::disk_access()
            .open(device, Access::ReadWriteExclusive)
            .context("failed to open device (need elevated privileges?)")?;
        let (boot, data) = layout::write_boot_data_layout(&mut *target, scheme, boot_bytes)?;
        eprintln!(
            "Layout: boot FAT32 {} + ext4 persistence {} (label {persist_label}).",
            humanize_bytes(boot.len),
            humanize_bytes(data.len)
        );
        let stats = reader.install_to_region(&mut *target, boot, label, kind, reporter)?;
        target.sync()?;
        stats
    };

    // 2) Re-read the partition table so /dev/sdX2 appears.
    let _ = std::process::Command::new("partprobe")
        .arg(&device.path)
        .status();
    let _ = std::process::Command::new("udevadm").arg("settle").status();
    let data_part = partition_path(&device.path, 2);
    let node = std::path::Path::new(&data_part);
    for _ in 0..50 {
        if node.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !node.exists() {
        bail!("persistence partition node {data_part} did not appear");
    }

    // 3) Format the persistence partition ext4 with the overlay label.
    eprintln!("Formatting {data_part} as ext4 (label {persist_label}) …");
    run_tool("mkfs.ext4", &["-F", "-q", "-L", persist_label, &data_part])
        .context("mkfs.ext4 failed")?;

    // 4) Debian live-boot needs a persistence.conf in the overlay.
    if kind.map(|k| k.needs_conf()).unwrap_or(false) {
        let mnt = std::env::temp_dir().join(format!("usbforge_persist_{}", std::process::id()));
        std::fs::create_dir_all(&mnt)?;
        let mnt_str = mnt.to_string_lossy().to_string();
        run_tool("mount", &[&data_part, &mnt_str])
            .context("mounting persistence partition failed")?;
        let write_res = std::fs::write(mnt.join("persistence.conf"), b"/ union\n");
        let _ = std::process::Command::new("sync").status();
        let _ = std::process::Command::new("umount").arg(&mnt_str).status();
        let _ = std::fs::remove_dir_all(&mnt);
        write_res.context("writing persistence.conf failed")?;
    }

    Ok(format!(
        "copied {} files ({}) + created ext4 persistence ({persist_label})",
        extract.files,
        humanize_bytes(extract.bytes)
    ))
}

/// Loop-mount an ISO read-only (UDF view preferred, so > 4 GiB files come
/// through; falls back to autodetect for plain ISO9660) and copy its tree into
/// `dest`. Returns `(file_count, byte_total)`.
fn mount_copy_iso(
    iso_path: &str,
    dest: &std::path::Path,
    reporter: &CliReporter,
) -> Result<(u64, u64)> {
    let mnt = std::env::temp_dir().join(format!("usbforge_src_{}", std::process::id()));
    std::fs::create_dir_all(&mnt)?;
    let mnt_str = mnt.to_string_lossy().to_string();

    // Probe the UDF view first (quietly — it's expected to fail on plain ISO9660).
    let udf_ok = std::process::Command::new("mount")
        .args(["-t", "udf", "-o", "loop,ro", iso_path, &mnt_str])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !udf_ok {
        run_tool("mount", &["-o", "loop,ro", iso_path, &mnt_str])
            .context("loop-mounting the source ISO failed")?;
    }

    let total = dir_size(&mnt);
    let mut done = 0u64;
    let mut files = 0u64;
    let result = copy_tree(&mnt, dest, total, &mut done, &mut files, reporter);

    let _ = std::process::Command::new("umount").arg(&mnt_str).status();
    let _ = std::fs::remove_dir_all(&mnt);

    result?;
    Ok((files, done))
}

/// Recursively copy `src` into `dst`, reporting progress against `total` bytes.
fn copy_tree(
    src: &std::path::Path,
    dst: &std::path::Path,
    total: u64,
    done: &mut u64,
    files: &mut u64,
    reporter: &CliReporter,
) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_tree(&path, &target, total, done, files, reporter)?;
        } else if ft.is_file() {
            std::fs::copy(&path, &target).with_context(|| format!("copying {}", path.display()))?;
            *files += 1;
            *done += entry.metadata().map(|m| m.len()).unwrap_or(0);
            if total > 0 {
                reporter.progress("extract", *done as f32 / total as f32);
            }
        }
    }
    Ok(())
}

/// Recursively sum the byte size of all regular files under `p`.
fn dir_size(p: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(p) {
        for e in entries.flatten() {
            match e.file_type() {
                Ok(ft) if ft.is_dir() => total += dir_size(&e.path()),
                Ok(ft) if ft.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    total
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
