//! Windows backend.
//!
//! Device enumeration probes `\\.\PhysicalDriveN` and queries
//! `IOCTL_STORAGE_QUERY_PROPERTY` / `IOCTL_DISK_GET_LENGTH_INFO`. Disk access
//! opens the physical drive with `CreateFileW` and drives it with `ReadFile` /
//! `WriteFile` / `SetFilePointerEx` / `DeviceIoControl`, locking and dismounting
//! the disk's volumes first for an exclusive write (the Windows equivalent of
//! Rufus `drive.c`).
//!
//! This code is compiled only for Windows targets; on Linux it is
//! `cfg`-excluded. It has been type-checked against `x86_64-pc-windows-gnu` but
//! its runtime behaviour is exercised on real Windows.

mod block;
mod enumerate;

use usbforge_core::device::DeviceEnumerator;
use usbforge_core::disk::DiskAccess;

pub fn enumerator() -> Box<dyn DeviceEnumerator> {
    Box::new(enumerate::WindowsEnumerator)
}

pub fn disk_access() -> Box<dyn DiskAccess> {
    Box::new(block::WindowsDiskAccess)
}

/// Null-terminated UTF-16 buffer for a Win32 wide-string path.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
