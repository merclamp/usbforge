//! Linux backend.
//!
//! Device enumeration is done by walking `/sys/block` (no external `libudev`
//! dependency for the basic listing — sysfs gives us everything the device
//! picker needs). Disk access opens `/dev/<name>` as a regular file; the
//! sector-size/flush capability calls will grow `ioctl(BLK*)` and `BLKFLSBUF`
//! as the write path is built out.

mod block;
mod sysfs;

use usbforge_core::device::DeviceEnumerator;
use usbforge_core::disk::DiskAccess;

pub fn enumerator() -> Box<dyn DeviceEnumerator> {
    Box::new(sysfs::SysfsEnumerator::new())
}

pub fn disk_access() -> Box<dyn DiskAccess> {
    Box::new(block::LinuxDiskAccess)
}
