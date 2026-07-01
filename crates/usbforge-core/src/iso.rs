//! ISO9660 reading, bootability analysis, and tree extraction
//! (Rufus `iso.c` / `ImageScanThread`).
//!
//! Reads an ISO with our own pure-Rust reader ([`crate::iso9660`]) and copies
//! its whole directory tree into a FAT filesystem via `fatfs` — the basis of
//! "file-copy" bootable media. For UEFI machines that alone is enough: firmware
//! boots `/EFI/BOOT/BOOT*.EFI` straight from the FAT32 partition.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fatfs::{Dir, FileSystem, FsOptions, ReadWriteSeek};

use crate::disk::BlockDevice;
use crate::filesystem::{FileSystem as Fs, PartitionScheme};
use crate::format::{format_fat32, PartitionSlice};
use crate::iso9660::{DirEntry, Iso};
use crate::layout;
use crate::report::{Reporter, ReporterExt};
use crate::{Error, Result};

/// An opened ISO9660 image.
pub struct IsoReader {
    iso: Iso,
    path: PathBuf,
}

/// What we learned about an ISO by scanning it.
#[derive(Debug, Clone, Default)]
pub struct IsoReport {
    pub volume_label: String,
    pub total_bytes: u64,
    pub total_files: u64,
    /// EFI boot architectures found under `/EFI/BOOT` (e.g. `"x64"`, `"aa64"`).
    pub uefi_archs: Vec<String>,
    /// Looks like a Windows installer (has `sources/install.wim`/`.esd`).
    pub windows_installer: bool,
    /// Detected BIOS bootloader, if any (`"isolinux"` / `"grub"`).
    pub bios_bootloader: Option<String>,
    /// The ISO is "isohybrid": it carries a real MBR boot sector, so writing it
    /// raw (dd) to a USB yields a drive that boots on BIOS *and* UEFI.
    pub isohybrid: bool,
    /// The image uses UDF (so it may hold files > 4 GiB, e.g. Windows ISOs).
    pub udf: bool,
    /// Detected live-ISO persistence family, if any (Ubuntu casper / Debian live).
    pub persistence: Option<PersistenceKind>,
}

impl IsoReport {
    pub fn is_uefi_bootable(&self) -> bool {
        !self.uefi_archs.is_empty()
    }
}

/// Live-ISO family that supports a persistence overlay, and how to label it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceKind {
    /// Ubuntu / casper — an ext4 partition labelled `casper-rw`.
    Casper,
    /// Debian / live-boot — an ext4 partition labelled `persistence` plus a
    /// `persistence.conf` containing `/ union`.
    LiveBoot,
}

impl PersistenceKind {
    pub fn label(self) -> &'static str {
        match self {
            PersistenceKind::Casper => "casper-rw",
            PersistenceKind::LiveBoot => "persistence",
        }
    }
    /// live-boot needs a `persistence.conf` file in the overlay; casper does not.
    pub fn needs_conf(self) -> bool {
        matches!(self, PersistenceKind::LiveBoot)
    }
}

/// Detect whether an image uses **UDF** — including ISO9660+UDF "bridge" images,
/// which is how Windows ISOs store an `install.wim` larger than ISO9660's 4 GiB
/// per-file limit. Looks for an `NSR02`/`NSR03` standard identifier in the Volume
/// Recognition Sequence (the 2048-byte sectors starting at sector 16). No root
/// needed. Returns `false` on any I/O error.
pub fn is_udf(path: impl AsRef<Path>) -> bool {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    if file.seek(SeekFrom::Start(16 * 2048)).is_err() {
        return false;
    }
    let mut buf = vec![0u8; 16 * 2048];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return false,
    };
    buf[..n].windows(5).any(|w| w == b"NSR02" || w == b"NSR03")
}

/// Cheap isohybrid check: read LBA0 and look for an MBR boot signature
/// (`0x55AA`) plus non-empty boot code. Plain ISO9660 leaves the first 16
/// sectors (the "system area") zeroed, so a populated MBR here means isohybrid.
/// Returns `false` on any I/O error.
pub fn is_isohybrid(path: impl AsRef<Path>) -> bool {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut lba0 = [0u8; 512];
    if file.read_exact(&mut lba0).is_err() {
        return false;
    }
    let has_signature = lba0[510] == 0x55 && lba0[511] == 0xAA;
    let has_bootcode = lba0[..440].iter().any(|&b| b != 0);
    has_signature && has_bootcode
}

/// Result of an extraction pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtractStats {
    pub files: u64,
    pub bytes: u64,
}

impl IsoReader {
    pub fn open(path: impl AsRef<Path>) -> Result<IsoReader> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let iso = Iso::open(file)?;
        Ok(IsoReader { iso, path })
    }

    /// Scan the image: volume label, total size/file count, and bootability.
    pub fn report(&self) -> IsoReport {
        let (total_files, total_bytes) = dir_totals(&self.iso, &self.iso.root());
        IsoReport {
            volume_label: self.iso.volume_label().to_string(),
            total_bytes,
            total_files,
            uefi_archs: self.detect_uefi(),
            windows_installer: self.exists_any(&["/sources/install.wim", "/sources/install.esd"]),
            bios_bootloader: self.detect_bios_bootloader(),
            isohybrid: is_isohybrid(&self.path),
            udf: is_udf(&self.path),
            persistence: self.detect_persistence(),
        }
    }

    fn detect_persistence(&self) -> Option<PersistenceKind> {
        if self.exists("/casper") {
            Some(PersistenceKind::Casper)
        } else if self.exists("/live") {
            Some(PersistenceKind::LiveBoot)
        } else {
            None
        }
    }

    /// Copy the entire ISO tree into the root of `fs`.
    pub fn extract_to_fat<W: ReadWriteSeek>(
        &self,
        fs: &FileSystem<W>,
        reporter: &dyn Reporter,
    ) -> Result<ExtractStats> {
        let (_, total) = dir_totals(&self.iso, &self.iso.root());
        reporter.info(&format!(
            "Extracting {} of files to the target filesystem …",
            crate::device::humanize_bytes(total)
        ));
        let mut stats = ExtractStats::default();
        copy_dir(
            &self.iso,
            &self.iso.root(),
            &fs.root_dir(),
            total,
            &mut stats,
            reporter,
        )?;
        reporter.progress("extract", 1.0);
        Ok(stats)
    }

    /// Full file-copy install: partition the device (single data partition),
    /// FAT32-format it, and extract the ISO tree onto it. The result is
    /// UEFI-bootable when the ISO carries `/EFI/BOOT/BOOT*.EFI`.
    pub fn install_to_device(
        &self,
        target: &mut dyn BlockDevice,
        scheme: PartitionScheme,
        label: &str,
        reporter: &dyn Reporter,
    ) -> Result<ExtractStats> {
        // EFI System Partition so UEFI firmware boots /EFI/BOOT/BOOT*.EFI.
        let region = layout::write_single_partition(target, scheme, Fs::Fat32, label, true)?;
        reporter.info(&format!(
            "Partition + FAT32 created at offset {} ({}).",
            region.start,
            crate::device::humanize_bytes(region.len)
        ));
        self.install_to_region(target, region, label, None, reporter)
    }

    /// Format an existing partition region as FAT32 and extract the ISO tree
    /// into it. Used both for the single-partition install and as the boot
    /// partition of a persistence layout. When `persistence` is set, the live
    /// distro's boot configs are patched to enable the overlay.
    pub fn install_to_region(
        &self,
        target: &mut dyn BlockDevice,
        region: layout::PartitionRegion,
        label: &str,
        persistence: Option<PersistenceKind>,
        reporter: &dyn Reporter,
    ) -> Result<ExtractStats> {
        let mut slice = PartitionSlice::new(target, region.start, region.len);
        format_fat32(&mut slice, label)?;
        let fs = FileSystem::new(&mut slice, fat_options())
            .map_err(|e| Error::Other(format!("mounting new FAT volume: {e}")))?;
        let stats = self.extract_to_fat(&fs, reporter)?;
        if let Some(kind) = persistence {
            match inject_persistence_param(&fs, kind) {
                Ok(0) => reporter.warn(
                    "No boot config found to enable persistence (overlay partition still created).",
                ),
                Ok(n) => reporter.info(&format!("Enabled persistence in {n} boot config(s).")),
                Err(e) => reporter.warn(&format!("Persistence boot-config edit failed: {e}")),
            }
        }
        fs.unmount()
            .map_err(|e| Error::Other(format!("finalising FAT volume: {e}")))?;
        Ok(stats)
    }

    /// Extract the whole ISO tree into an existing directory on a mounted
    /// filesystem (used for the UEFI:NTFS path, where the target NTFS volume is
    /// mounted by the OS and we copy into it).
    pub fn extract_to_dir(&self, dir: &Path, reporter: &dyn Reporter) -> Result<ExtractStats> {
        let (_, total) = dir_totals(&self.iso, &self.iso.root());
        reporter.info(&format!(
            "Extracting {} to the mounted volume …",
            crate::device::humanize_bytes(total)
        ));
        let mut stats = ExtractStats::default();
        copy_dir_to_fs(
            &self.iso,
            &self.iso.root(),
            dir,
            total,
            &mut stats,
            reporter,
        )?;
        reporter.progress("extract", 1.0);
        Ok(stats)
    }

    fn exists(&self, path: &str) -> bool {
        matches!(self.iso.find(path), Ok(Some(_)))
    }

    fn exists_any(&self, paths: &[&str]) -> bool {
        paths.iter().any(|p| self.exists(p))
    }

    fn detect_uefi(&self) -> Vec<String> {
        const CANDIDATES: [(&str, &str); 4] = [
            ("BOOTX64.EFI", "x64"),
            ("BOOTIA32.EFI", "ia32"),
            ("BOOTAA64.EFI", "aa64"),
            ("BOOTARM.EFI", "arm"),
        ];
        let mut found = Vec::new();
        for (fname, arch) in CANDIDATES {
            // Lookup is case-insensitive, so one spelling suffices.
            if self.exists(&format!("/EFI/BOOT/{fname}")) {
                found.push(arch.to_string());
            }
        }
        found
    }

    fn detect_bios_bootloader(&self) -> Option<String> {
        if self.exists("/isolinux/isolinux.bin") {
            Some("isolinux".to_string())
        } else if self.exists("/boot/grub/i386-pc") || self.exists("/boot/grub") {
            Some("grub".to_string())
        } else {
            None
        }
    }
}

/// Recursively copy an ISO directory into a fatfs directory.
fn copy_dir<W: ReadWriteSeek>(
    iso: &Iso,
    dir: &DirEntry,
    dst: &Dir<'_, W>,
    total: u64,
    stats: &mut ExtractStats,
    reporter: &dyn Reporter,
) -> Result<()> {
    for entry in iso.read_dir(dir)? {
        if entry.name.is_empty() {
            continue;
        }
        if entry.is_dir {
            let sub = dst
                .create_dir(&entry.name)
                .map_err(|e| Error::Other(format!("creating dir {:?}: {e}", entry.name)))?;
            copy_dir(iso, &entry, &sub, total, stats, reporter)?;
        } else {
            let mut writer = dst
                .create_file(&entry.name)
                .map_err(|e| Error::Other(format!("creating file {:?}: {e}", entry.name)))?;
            iso.copy_file(&entry, &mut writer)?;
            stats.files += 1;
            stats.bytes += u64::from(entry.size);
            if total > 0 {
                reporter.progress("extract", stats.bytes as f32 / total as f32);
            }
        }
    }
    Ok(())
}

/// Recursively copy an ISO directory into a real filesystem directory.
fn copy_dir_to_fs(
    iso: &Iso,
    dir: &DirEntry,
    dst: &Path,
    total: u64,
    stats: &mut ExtractStats,
    reporter: &dyn Reporter,
) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in iso.read_dir(dir)? {
        if entry.name.is_empty() {
            continue;
        }
        let path = dst.join(&entry.name);
        if entry.is_dir {
            copy_dir_to_fs(iso, &entry, &path, total, stats, reporter)?;
        } else {
            let mut writer = std::fs::File::create(&path)
                .map_err(|e| Error::Other(format!("creating {:?}: {e}", entry.name)))?;
            iso.copy_file(&entry, &mut writer)?;
            stats.files += 1;
            stats.bytes += u64::from(entry.size);
            if total > 0 {
                reporter.progress("extract", stats.bytes as f32 / total as f32);
            }
        }
    }
    Ok(())
}

/// Recursively sum (file_count, byte_total) of a directory tree.
fn dir_totals(iso: &Iso, dir: &DirEntry) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    if let Ok(entries) = iso.read_dir(dir) {
        for entry in entries {
            if entry.is_dir {
                let (f, b) = dir_totals(iso, &entry);
                files += f;
                bytes += b;
            } else {
                files += 1;
                bytes += u64::from(entry.size);
            }
        }
    }
    (files, bytes)
}

/// Boot configs commonly carrying the live kernel command line (paths are
/// matched case-insensitively by FAT).
const BOOT_CONFIGS: [&str; 9] = [
    "boot/grub/grub.cfg",
    "boot/grub/loopback.cfg",
    "EFI/BOOT/grub.cfg",
    "isolinux/isolinux.cfg",
    "isolinux/txt.cfg",
    "boot/isolinux/isolinux.cfg",
    "syslinux/syslinux.cfg",
    "syslinux/txt.cfg",
    "boot/syslinux/syslinux.cfg",
];

/// Patch the live distro's boot configs on `fs` to add the persistence kernel
/// parameter (`persistent` for casper, `persistence` for live-boot). Returns the
/// number of config files changed.
fn inject_persistence_param<W: ReadWriteSeek>(
    fs: &FileSystem<W>,
    kind: PersistenceKind,
) -> Result<usize> {
    let param = match kind {
        PersistenceKind::Casper => "persistent",
        PersistenceKind::LiveBoot => "persistence",
    };
    let root = fs.root_dir();
    let mut edited = 0;
    for path in BOOT_CONFIGS {
        if edit_boot_config(&root, path, param)? {
            edited += 1;
        }
    }
    Ok(edited)
}

/// Read a config file, add `param` to its kernel command lines, write it back.
/// Returns `Ok(false)` if the file is absent or already contained the param.
fn edit_boot_config<W: ReadWriteSeek>(root: &Dir<'_, W>, path: &str, param: &str) -> Result<bool> {
    let mut file = match root.open_file(path) {
        Ok(f) => f,
        Err(_) => return Ok(false),
    };
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() {
        return Ok(false);
    }
    let patched = add_kernel_param(&content, param);
    if patched == content {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| Error::Other(format!("seek {path}: {e}")))?;
    file.truncate()
        .map_err(|e| Error::Other(format!("truncate {path}: {e}")))?;
    file.write_all(patched.as_bytes())
        .map_err(|e| Error::Other(format!("write {path}: {e}")))?;
    Ok(true)
}

/// Append `param` to every kernel-loading line (`linux`/`linux16`/`append`/
/// `kernel`) that doesn't already carry it. casper/live-boot scan the whole
/// `/proc/cmdline`, so appending at end of line is sufficient.
fn add_kernel_param(content: &str, param: &str) -> String {
    let mut out = String::with_capacity(content.len() + 32);
    for line in content.lines() {
        let lower = line.trim_start().to_ascii_lowercase();
        let is_kernel_line = lower.starts_with("linux ")
            || lower.starts_with("linux\t")
            || lower.starts_with("linux16 ")
            || lower.starts_with("append ")
            || lower.starts_with("append\t")
            || lower.starts_with("kernel ");
        let has_param = line.split_whitespace().any(|t| t == param);
        if is_kernel_line && !has_param {
            out.push_str(line.trim_end());
            out.push(' ');
            out.push_str(param);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Convenience for the CLI `inspect` path: open an ISO and return its report.
pub fn report_for(path: impl AsRef<Path>) -> Result<IsoReport> {
    Ok(IsoReader::open(path)?.report())
}

fn fat_options() -> FsOptions {
    FsOptions::new().update_accessed_date(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::NullReporter;
    use crate::testutil::MemDevice;
    use std::process::Command;

    fn have_xorrisofs() -> bool {
        Command::new("xorrisofs")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn isohybrid_detection() {
        let p = std::env::temp_dir().join(format!("usbforge_isohybrid_{}.bin", std::process::id()));
        let mut buf = vec![0u8; 2048];
        buf[0] = 0xEB;
        buf[1] = 0x63;
        buf[510] = 0x55;
        buf[511] = 0xAA;
        std::fs::write(&p, &buf).unwrap();
        assert!(is_isohybrid(&p));

        std::fs::write(&p, vec![0u8; 2048]).unwrap();
        assert!(!is_isohybrid(&p));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn kernel_param_injection_string() {
        let grub = "menuentry 'Live' {\n  linux /casper/vmlinuz boot=casper quiet splash ---\n  initrd /casper/initrd\n}\n";
        let out = add_kernel_param(grub, "persistent");
        assert!(out.contains("quiet splash --- persistent"));
        assert!(out.contains("initrd /casper/initrd"));
        assert_eq!(add_kernel_param(&out, "persistent"), out);

        let syslinux = "label live\n  append boot=casper quiet ---\n";
        assert!(add_kernel_param(syslinux, "persistent").contains("quiet --- persistent"));
    }

    #[test]
    fn inject_persistence_into_fat() {
        let mut dev = MemDevice::new(64 * 1024 * 1024);
        let cfg = "menuentry 'L' {\n  linux /casper/vmlinuz boot=casper quiet splash\n}\n";

        let len = dev.size();
        {
            let mut slice = PartitionSlice::new(&mut dev, 0, len);
            format_fat32(&mut slice, "TEST").unwrap();
            let fs = FileSystem::new(&mut slice, fat_options()).unwrap();
            {
                let grub = fs
                    .root_dir()
                    .create_dir("boot")
                    .unwrap()
                    .create_dir("grub")
                    .unwrap();
                grub.create_file("grub.cfg")
                    .unwrap()
                    .write_all(cfg.as_bytes())
                    .unwrap();
            }
            let n = inject_persistence_param(&fs, PersistenceKind::Casper).unwrap();
            assert_eq!(n, 1);
            fs.unmount().unwrap();
        }

        let len = dev.size();
        let mut slice = PartitionSlice::new(&mut dev, 0, len);
        let fs = FileSystem::new(&mut slice, fat_options()).unwrap();
        let mut s = String::new();
        fs.root_dir()
            .open_file("boot/grub/grub.cfg")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert!(s.contains("quiet splash persistent"));
    }

    #[test]
    fn udf_detection() {
        let p = std::env::temp_dir().join(format!("usbforge_udf_{}.bin", std::process::id()));
        let mut buf = vec![0u8; 20 * 2048];
        buf[17 * 2048] = 0x00;
        buf[17 * 2048 + 1..17 * 2048 + 6].copy_from_slice(b"NSR02");
        std::fs::write(&p, &buf).unwrap();
        assert!(is_udf(&p));

        std::fs::write(&p, vec![0u8; 20 * 2048]).unwrap();
        assert!(!is_udf(&p));
        let _ = std::fs::remove_file(&p);
    }

    /// End-to-end: build a tiny ISO, read it with our reader, extract into a
    /// FAT32 volume, and verify the files. Skipped when `xorrisofs` is absent.
    #[test]
    fn iso_extract_to_fat_roundtrip() {
        if !have_xorrisofs() {
            eprintln!("skipping iso_extract_to_fat_roundtrip: xorrisofs not found");
            return;
        }

        let base = std::env::temp_dir().join(format!("usbforge_iso_e2e_{}", std::process::id()));
        let src = base.join("src");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(src.join("EFI/BOOT")).unwrap();
        std::fs::create_dir_all(src.join("DIR1")).unwrap();
        std::fs::write(src.join("README.TXT"), b"hello").unwrap();
        std::fs::write(src.join("EFI/BOOT/BOOTX64.EFI"), b"MZ-fake-efi").unwrap();
        std::fs::write(src.join("DIR1/NESTED.TXT"), b"nested").unwrap();

        let iso_path = base.join("test.iso");
        let status = Command::new("xorrisofs")
            .args(["-quiet", "-J", "-R", "-V", "TESTLABEL", "-o"])
            .arg(&iso_path)
            .arg(&src)
            .status()
            .unwrap();
        assert!(status.success(), "xorrisofs failed");

        let reader = IsoReader::open(&iso_path).unwrap();
        let report = reader.report();
        assert_eq!(report.volume_label, "TESTLABEL");
        assert!(report.uefi_archs.iter().any(|a| a == "x64"));
        assert_eq!(report.total_files, 3);

        let mut dev = MemDevice::new(64 * 1024 * 1024);
        let len = dev.size();
        {
            let mut slice = PartitionSlice::new(&mut dev, 0, len);
            format_fat32(&mut slice, "TESTLABEL").unwrap();
            let fs = FileSystem::new(&mut slice, fat_options()).unwrap();
            reader.extract_to_fat(&fs, &NullReporter).unwrap();
            fs.unmount().unwrap();
        }

        let len = dev.size();
        let mut slice = PartitionSlice::new(&mut dev, 0, len);
        let fs = FileSystem::new(&mut slice, fat_options()).unwrap();
        let root = fs.root_dir();

        let mut s = String::new();
        root.open_file("README.TXT")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert_eq!(s, "hello");

        let mut s2 = String::new();
        root.open_file("DIR1/NESTED.TXT")
            .unwrap()
            .read_to_string(&mut s2)
            .unwrap();
        assert_eq!(s2, "nested");

        assert!(root.open_file("EFI/BOOT/BOOTX64.EFI").is_ok());

        let _ = std::fs::remove_dir_all(&base);
    }
}
