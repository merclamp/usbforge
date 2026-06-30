//! USBForge command-line frontend.
//!
//! A thin shell over `usbforge-core` + `usbforge-platform`. It doubles as the
//! proof-of-concept for the portable core: `list` exercises device enumeration,
//! `hash`/`inspect` exercise the image + hashing modules — all without any GUI.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use usbforge_core::device::Device;
use usbforge_core::hash::{self, Algo};
use usbforge_core::image::ImageInfo;
use usbforge_core::report::{Level, Reporter};
use usbforge_core::PRODUCT;

#[derive(Parser)]
#[command(name = "usbforge", version, about = "Cross-platform bootable USB / disk image writer")]
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::List { all } => cmd_list(all),
        Command::Inspect { path } => cmd_inspect(&path),
        Command::Hash { path, algo } => cmd_hash(&path, &algo),
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
        "{:<14} {:>8} {:>8} {:<3} {:<3} {}",
        "PATH", "BUS", "SIZE", "RM", "RO", "NAME"
    );
    for d in devices {
        println!(
            "{:<14} {:>8} {:>8} {:<3} {:<3} {}",
            d.path,
            d.bus.to_string(),
            d.size_human(),
            if d.removable { "yes" } else { "no" },
            if d.read_only { "yes" } else { "no" },
            d.display_name(),
        );
    }
}

fn cmd_inspect(path: &str) -> Result<()> {
    let info: ImageInfo = ImageInfo::inspect(path).context("failed to inspect image")?;
    println!("{} — {}", PRODUCT, "image inspection");
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
        eprint!("\r{operation}: {:>3.0}%", (fraction * 100.0).clamp(0.0, 100.0));
    }
}
