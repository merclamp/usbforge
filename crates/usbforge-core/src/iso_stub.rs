//! Non-Unix stub for the `iso` module.
//!
//! The full ISO reader lives in `iso.rs` and depends on `cdfs`, which pulls the
//! Unix-only `fuser` crate and therefore can't build on Windows. This stub keeps
//! the cross-platform helpers (`is_udf`, `is_isohybrid`, the report/stat types)
//! and provides an `IsoReader` whose methods report that ISO create/inspect is
//! not available on this platform yet, so the CLI still builds and runs (with
//! `list` / `write` / `format` / `download` working).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::disk::BlockDevice;
use crate::filesystem::PartitionScheme;
use crate::layout;
use crate::report::Reporter;
use crate::{Error, Result};

/// Live-ISO family that supports a persistence overlay, and how to label it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceKind {
    Casper,
    LiveBoot,
}

impl PersistenceKind {
    pub fn label(self) -> &'static str {
        match self {
            PersistenceKind::Casper => "casper-rw",
            PersistenceKind::LiveBoot => "persistence",
        }
    }
    pub fn needs_conf(self) -> bool {
        matches!(self, PersistenceKind::LiveBoot)
    }
}

/// What we learned about an ISO by scanning it.
#[derive(Debug, Clone, Default)]
pub struct IsoReport {
    pub volume_label: String,
    pub total_bytes: u64,
    pub total_files: u64,
    pub uefi_archs: Vec<String>,
    pub windows_installer: bool,
    pub bios_bootloader: Option<String>,
    pub isohybrid: bool,
    pub udf: bool,
    pub persistence: Option<PersistenceKind>,
}

impl IsoReport {
    pub fn is_uefi_bootable(&self) -> bool {
        !self.uefi_archs.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ExtractStats {
    pub files: u64,
    pub bytes: u64,
}

/// Detect UDF via the `NSR02`/`NSR03` Volume Recognition Sequence marker.
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

/// Detect an isohybrid ISO (a real MBR boot sector at LBA0).
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

/// Stub ISO reader — ISO9660 reading isn't available on this platform yet.
pub struct IsoReader;

impl IsoReader {
    pub fn open(_path: impl AsRef<Path>) -> Result<IsoReader> {
        Err(Error::unsupported(
            "ISO reading (create / ISO inspect) is not available on this platform yet",
        ))
    }

    pub fn report(&self) -> IsoReport {
        IsoReport::default()
    }

    pub fn install_to_device(
        &self,
        _target: &mut dyn BlockDevice,
        _scheme: PartitionScheme,
        _label: &str,
        _reporter: &dyn Reporter,
    ) -> Result<ExtractStats> {
        Err(Error::unsupported(
            "ISO install is not available on this platform yet",
        ))
    }

    pub fn install_to_region(
        &self,
        _target: &mut dyn BlockDevice,
        _region: layout::PartitionRegion,
        _label: &str,
        _persistence: Option<PersistenceKind>,
        _reporter: &dyn Reporter,
    ) -> Result<ExtractStats> {
        Err(Error::unsupported(
            "ISO install is not available on this platform yet",
        ))
    }
}
