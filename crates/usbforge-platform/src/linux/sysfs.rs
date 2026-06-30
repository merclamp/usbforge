//! Device enumeration by walking `/sys/block`.

use std::fs;
use std::path::Path;

use usbforge_core::device::{Bus, Device, DeviceEnumerator};
use usbforge_core::Result;

pub struct SysfsEnumerator {
    sys_block: std::path::PathBuf,
}

impl SysfsEnumerator {
    pub fn new() -> Self {
        SysfsEnumerator {
            sys_block: Path::new("/sys/block").to_path_buf(),
        }
    }
}

impl DeviceEnumerator for SysfsEnumerator {
    fn list(&self, only_removable: bool) -> Result<Vec<Device>> {
        let mut devices = Vec::new();

        let entries = match fs::read_dir(&self.sys_block) {
            Ok(e) => e,
            // No /sys/block (non-Linux container?) -> empty list, not an error.
            Err(_) => return Ok(devices),
        };

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_pseudo_device(&name) {
                continue;
            }
            let base = self.sys_block.join(&name);
            if let Some(dev) = read_device(&base, &name) {
                if !only_removable || dev.is_removable_media() {
                    devices.push(dev);
                }
            }
        }

        devices.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(devices)
    }
}

/// Skip loop/ram/zram/device-mapper and similar virtual nodes.
fn is_pseudo_device(name: &str) -> bool {
    const PREFIXES: [&str; 6] = ["loop", "ram", "zram", "dm-", "md", "sr"];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

fn read_device(base: &Path, name: &str) -> Option<Device> {
    // size is reported in 512-byte sectors regardless of logical block size.
    let sectors: u64 = read_trim(&base.join("size"))?.parse().ok()?;
    if sectors == 0 {
        return None;
    }
    let logical_sector_size: u32 = read_trim(&base.join("queue/logical_block_size"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(512);
    let size = sectors * 512;

    let removable = read_trim(&base.join("removable")).as_deref() == Some("1");
    let read_only = read_trim(&base.join("ro")).as_deref() == Some("1");

    let vendor = read_trim(&base.join("device/vendor")).unwrap_or_default();
    let model = read_trim(&base.join("device/model")).unwrap_or_default();
    let serial = read_trim(&base.join("device/serial")).filter(|s| !s.is_empty());

    let bus = detect_bus(base, name);

    Some(Device {
        id: name.to_string(),
        path: format!("/dev/{name}"),
        vendor,
        model,
        serial,
        size,
        logical_sector_size,
        bus,
        removable,
        read_only,
    })
}

/// Infer the transport bus from the resolved sysfs device path plus the kernel
/// name. `/sys/block/<name>` is a symlink into the real device tree, so its
/// canonical path reveals whether we traversed a `usb`, `nvme`, `mmc`, ... node.
fn detect_bus(base: &Path, name: &str) -> Bus {
    if name.starts_with("nvme") {
        return Bus::Nvme;
    }
    if name.starts_with("mmcblk") {
        return Bus::Mmc;
    }
    if name.starts_with("vd") {
        return Bus::Virtual;
    }

    if let Ok(real) = fs::canonicalize(base) {
        let p = real.to_string_lossy();
        if p.contains("/usb") {
            return Bus::Usb;
        }
        if p.contains("/nvme") {
            return Bus::Nvme;
        }
        if p.contains("/mmc") {
            return Bus::Mmc;
        }
        if p.contains("/ata") {
            return Bus::Sata;
        }
        if p.contains("/virtio") {
            return Bus::Virtual;
        }
        if p.contains("/scsi") || p.contains("/host") {
            return Bus::Scsi;
        }
    }
    Bus::Unknown
}

/// Read a sysfs attribute and trim trailing whitespace/newline. Returns `None`
/// if the file is absent or unreadable.
fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}
