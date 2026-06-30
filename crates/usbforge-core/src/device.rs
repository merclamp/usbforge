//! Storage device model and enumeration trait.
//!
//! Maps to Rufus `dev.c` (`GetDevices()` via SetupAPI/CfgMgr on Windows) and the
//! identity half of `drive.c`. Platform backends turn OS-specific enumeration
//! (libudev/sysfs on Linux, SetupAPI on Windows) into a list of [`Device`].

use std::fmt;

/// Transport bus a device is attached through. Used both for display and for
/// the "is this a removable flash drive?" heuristic (cf. Rufus `hdd_vs_ufd.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    Usb,
    Sata,
    Nvme,
    Scsi,
    Mmc,
    Virtual,
    Unknown,
}

impl fmt::Display for Bus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Bus::Usb => "USB",
            Bus::Sata => "SATA",
            Bus::Nvme => "NVMe",
            Bus::Scsi => "SCSI",
            Bus::Mmc => "MMC/SD",
            Bus::Virtual => "Virtual",
            Bus::Unknown => "?",
        };
        f.write_str(s)
    }
}

/// A physical storage device candidate for writing.
#[derive(Debug, Clone)]
pub struct Device {
    /// Stable short id: `sdb` on Linux, `PhysicalDrive1` on Windows.
    pub id: String,
    /// OS path used to open the whole device: `/dev/sdb` or `\\.\PhysicalDrive1`.
    pub path: String,
    pub vendor: String,
    pub model: String,
    pub serial: Option<String>,
    /// Total capacity in bytes.
    pub size: u64,
    /// Logical sector size in bytes (usually 512 or 4096).
    pub logical_sector_size: u32,
    pub bus: Bus,
    /// Kernel "removable" flag.
    pub removable: bool,
    /// Device is write-protected.
    pub read_only: bool,
}

impl Device {
    /// `true` when this looks like a USB stick / SD card rather than a fixed
    /// system disk — the default filter so we never offer the user their own
    /// boot drive by accident.
    pub fn is_removable_media(&self) -> bool {
        self.removable || matches!(self.bus, Bus::Usb | Bus::Mmc)
    }

    /// Human-friendly capacity, 1024-based with conventional labels (matching
    /// how Rufus presents drive sizes).
    pub fn size_human(&self) -> String {
        humanize_bytes(self.size)
    }

    /// `"Vendor Model"` with redundant whitespace collapsed.
    pub fn display_name(&self) -> String {
        let name = format!("{} {}", self.vendor.trim(), self.model.trim());
        let name = name.trim();
        if name.is_empty() {
            self.id.clone()
        } else {
            name.to_string()
        }
    }
}

/// Enumerate attached storage devices. Implemented per-platform in
/// `usbforge-platform`.
pub trait DeviceEnumerator {
    /// List devices. When `only_removable` is set, fixed disks are filtered out
    /// (the safe default for a USB writer).
    fn list(&self, only_removable: bool) -> crate::Result<Vec<Device>>;
}

/// 1024-based size formatter (`14 GB`, `512 MB`, ...).
pub fn humanize_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else if value >= 100.0 {
        format!("{:.0} {}", value, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize() {
        assert_eq!(humanize_bytes(0), "0 B");
        assert_eq!(humanize_bytes(512), "512 B");
        assert_eq!(humanize_bytes(1024), "1.0 KB");
        assert_eq!(humanize_bytes(15_032_385_536), "14.0 GB");
    }
}
