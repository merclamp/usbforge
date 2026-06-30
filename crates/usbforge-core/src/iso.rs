//! ISO9660 reading, bootability analysis, and tree extraction
//! (Rufus `iso.c` / `ImageScanThread` + libcdio).
//!
//! Reads an ISO with the pure-Rust `cdfs` crate and can copy its whole directory
//! tree into a FAT filesystem via `fatfs` — the basis of "file-copy" bootable
//! media. For UEFI machines that alone is enough: firmware boots
//! `/EFI/BOOT/BOOT*.EFI` straight from the FAT32 partition, no bootloader install.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use cdfs::{DirectoryEntry, ISODirectory, ISO9660};
use fatfs::{Dir, FileSystem, FsOptions, ReadWriteSeek};

use crate::disk::BlockDevice;
use crate::filesystem::{FileSystem as Fs, PartitionScheme};
use crate::format::{format_fat32, PartitionSlice};
use crate::layout;
use crate::report::{Reporter, ReporterExt};
use crate::{Error, Result};

/// An opened ISO9660 image.
pub struct IsoReader {
    iso: ISO9660<File>,
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
}

impl IsoReport {
    pub fn is_uefi_bootable(&self) -> bool {
        !self.uefi_archs.is_empty()
    }
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
        let iso = ISO9660::new(file)
            .map_err(|e| Error::Image(format!("not a valid ISO9660 image: {e}")))?;
        Ok(IsoReader { iso, path })
    }

    /// Scan the image: volume label, total size/file count, and bootability.
    pub fn report(&self) -> IsoReport {
        let (total_files, total_bytes) = dir_totals(self.iso.root());
        IsoReport {
            volume_label: self.read_volume_label(),
            total_bytes,
            total_files,
            uefi_archs: self.detect_uefi(),
            windows_installer: self.exists_any(&["/sources/install.wim", "/sources/install.esd"]),
            bios_bootloader: self.detect_bios_bootloader(),
            isohybrid: is_isohybrid(&self.path),
        }
    }

    /// Copy the entire ISO tree into the root of `fs`.
    pub fn extract_to_fat<W: ReadWriteSeek>(
        &self,
        fs: &FileSystem<W>,
        reporter: &dyn Reporter,
    ) -> Result<ExtractStats> {
        let (_, total) = dir_totals(self.iso.root());
        reporter.info(&format!(
            "Extracting {} of files to the target filesystem …",
            crate::device::humanize_bytes(total)
        ));
        let mut stats = ExtractStats::default();
        copy_dir(self.iso.root(), &fs.root_dir(), total, &mut stats, reporter)?;
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
        let mut slice = PartitionSlice::new(target, region.start, region.len);
        format_fat32(&mut slice, label)?;
        let fs = FileSystem::new(&mut slice, fat_options())
            .map_err(|e| Error::Other(format!("mounting new FAT volume: {e}")))?;
        let stats = self.extract_to_fat(&fs, reporter)?;
        fs.unmount()
            .map_err(|e| Error::Other(format!("finalising FAT volume: {e}")))?;
        Ok(stats)
    }

    /// Extract the whole ISO tree into an existing directory on a mounted
    /// filesystem (used for the UEFI:NTFS path, where the target NTFS volume is
    /// mounted by the OS and we copy into it).
    pub fn extract_to_dir(&self, dir: &Path, reporter: &dyn Reporter) -> Result<ExtractStats> {
        let (_, total) = dir_totals(self.iso.root());
        reporter.info(&format!(
            "Extracting {} to the mounted volume …",
            crate::device::humanize_bytes(total)
        ));
        let mut stats = ExtractStats::default();
        copy_dir_to_fs(self.iso.root(), dir, total, &mut stats, reporter)?;
        reporter.progress("extract", 1.0);
        Ok(stats)
    }

    fn exists(&self, path: &str) -> bool {
        matches!(self.iso.open(path), Ok(Some(_)))
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
            // Try common case spellings (ISO9660 upper-cases; Joliet preserves).
            let variants = [
                format!("/EFI/BOOT/{fname}"),
                format!("/efi/boot/{}", fname.to_ascii_lowercase()),
            ];
            if variants.iter().any(|p| self.exists(p)) {
                found.push(arch.to_string());
            }
        }
        found
    }

    fn detect_bios_bootloader(&self) -> Option<String> {
        if self.exists_any(&["/isolinux/isolinux.bin", "/ISOLINUX/ISOLINUX.BIN"]) {
            Some("isolinux".to_string())
        } else if self.exists_any(&["/boot/grub/i386-pc", "/boot/grub"]) {
            Some("grub".to_string())
        } else {
            None
        }
    }

    /// Read the volume identifier straight from the Primary Volume Descriptor
    /// (LBA 16, offset 40, 32 bytes) — `cdfs` doesn't expose it directly.
    fn read_volume_label(&self) -> String {
        let mut f = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return String::new(),
        };
        if f.seek(SeekFrom::Start(16 * 2048 + 40)).is_err() {
            return String::new();
        }
        let mut buf = [0u8; 32];
        if f.read_exact(&mut buf).is_err() {
            return String::new();
        }
        String::from_utf8_lossy(&buf).trim().to_string()
    }
}

/// Recursively copy a cdfs directory into a fatfs directory.
fn copy_dir<W: ReadWriteSeek>(
    src: &ISODirectory<File>,
    dst: &Dir<'_, W>,
    total: u64,
    stats: &mut ExtractStats,
    reporter: &dyn Reporter,
) -> Result<()> {
    for entry in src.contents() {
        let entry = entry.map_err(|e| Error::Image(format!("ISO read error: {e}")))?;
        let name = clean_name(entry.identifier());
        if is_special(&name) {
            continue;
        }
        match entry {
            DirectoryEntry::Directory(dir) => {
                let sub = dst
                    .create_dir(&name)
                    .map_err(|e| Error::Other(format!("creating dir {name:?}: {e}")))?;
                copy_dir(&dir, &sub, total, stats, reporter)?;
            }
            DirectoryEntry::File(file) => {
                let mut writer = dst
                    .create_file(&name)
                    .map_err(|e| Error::Other(format!("creating file {name:?}: {e}")))?;
                let mut reader = file.read();
                std::io::copy(&mut reader, &mut writer)
                    .map_err(|e| Error::Other(format!("copying {name:?}: {e}")))?;
                stats.files += 1;
                stats.bytes += u64::from(file.size());
                if total > 0 {
                    reporter.progress("extract", stats.bytes as f32 / total as f32);
                }
            }
            // Symlinks aren't representable on FAT; skip them.
            _ => {}
        }
    }
    Ok(())
}

/// Recursively copy a cdfs directory into a real filesystem directory.
fn copy_dir_to_fs(
    src: &ISODirectory<File>,
    dst: &Path,
    total: u64,
    stats: &mut ExtractStats,
    reporter: &dyn Reporter,
) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in src.contents() {
        let entry = entry.map_err(|e| Error::Image(format!("ISO read error: {e}")))?;
        let name = clean_name(entry.identifier());
        if is_special(&name) {
            continue;
        }
        let path = dst.join(&name);
        match entry {
            DirectoryEntry::Directory(dir) => {
                copy_dir_to_fs(&dir, &path, total, stats, reporter)?;
            }
            DirectoryEntry::File(file) => {
                let mut writer = std::fs::File::create(&path)
                    .map_err(|e| Error::Other(format!("creating {name:?}: {e}")))?;
                let mut reader = file.read();
                std::io::copy(&mut reader, &mut writer)
                    .map_err(|e| Error::Other(format!("copying {name:?}: {e}")))?;
                stats.files += 1;
                stats.bytes += u64::from(file.size());
                if total > 0 {
                    reporter.progress("extract", stats.bytes as f32 / total as f32);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Recursively sum (file_count, byte_total) of a directory tree.
fn dir_totals(dir: &ISODirectory<File>) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in dir.contents().flatten() {
        if is_special(&clean_name(entry.identifier())) {
            continue;
        }
        match entry {
            DirectoryEntry::Directory(sub) => {
                let (f, b) = dir_totals(&sub);
                files += f;
                bytes += b;
            }
            DirectoryEntry::File(file) => {
                files += 1;
                bytes += u64::from(file.size());
            }
            _ => {}
        }
    }
    (files, bytes)
}

/// Strip the ISO9660 `;1` version suffix and any trailing dot.
fn clean_name(raw: &str) -> String {
    let base = raw.split(';').next().unwrap_or(raw);
    base.trim_end_matches('.').to_string()
}

/// Skip the `.`/`..` self/parent records (identifiers 0x00 / 0x01 in ISO9660).
fn is_special(name: &str) -> bool {
    name.is_empty()
        || name == "."
        || name == ".."
        || name.starts_with('\u{0}')
        || name.starts_with('\u{1}')
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
    fn clean_name_strips_version_and_dot() {
        assert_eq!(clean_name("README.TXT;1"), "README.TXT");
        assert_eq!(clean_name("DIR."), "DIR");
        assert_eq!(clean_name("file"), "file");
    }

    #[test]
    fn isohybrid_detection() {
        let p = std::env::temp_dir().join(format!("usbforge_isohybrid_{}.bin", std::process::id()));

        // MBR boot signature + non-zero boot code -> isohybrid.
        let mut buf = vec![0u8; 2048];
        buf[0] = 0xEB;
        buf[1] = 0x63;
        buf[510] = 0x55;
        buf[511] = 0xAA;
        std::fs::write(&p, &buf).unwrap();
        assert!(is_isohybrid(&p));

        // All zero (plain ISO system area) -> not isohybrid.
        std::fs::write(&p, vec![0u8; 2048]).unwrap();
        assert!(!is_isohybrid(&p));

        let _ = std::fs::remove_file(&p);
    }

    /// End-to-end: build a tiny ISO, read it, extract into a FAT32 volume, and
    /// verify the files came across. Skipped when `xorrisofs` is unavailable
    /// (e.g. on the Windows CI runner).
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

        // Read + scan.
        let reader = IsoReader::open(&iso_path).unwrap();
        let report = reader.report();
        assert_eq!(report.volume_label, "TESTLABEL");
        assert!(report.uefi_archs.iter().any(|a| a == "x64"));
        assert_eq!(report.total_files, 3);

        // Extract into a freshly FAT32-formatted in-memory device.
        let mut dev = MemDevice::new(64 * 1024 * 1024);
        let len = dev.size();
        {
            let mut slice = PartitionSlice::new(&mut dev, 0, len);
            format_fat32(&mut slice, "TESTLABEL").unwrap();
            let fs = FileSystem::new(&mut slice, fat_options()).unwrap();
            reader.extract_to_fat(&fs, &NullReporter).unwrap();
            fs.unmount().unwrap();
        }

        // Re-mount and verify the tree.
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
