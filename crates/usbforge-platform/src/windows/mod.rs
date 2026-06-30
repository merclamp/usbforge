//! Windows backend (skeleton).
//!
//! Device enumeration will use SetupAPI / CfgMgr (`SetupDiGetClassDevs`,
//! `CM_Get_*`) like Rufus `dev.c`; disk access will use `CreateFileW` on
//! `\\.\PhysicalDriveN` plus `DeviceIoControl` (`IOCTL_DISK_*`, `FSCTL_*`).
//! Those land via the `windows` crate behind the `cfg(windows)` dependency gate
//! in `Cargo.toml`. For now the methods return a clear `Unsupported` error so
//! the crate builds and links on Windows targets.

use usbforge_core::device::{Device, DeviceEnumerator};
use usbforge_core::disk::{Access, BlockDevice, DiskAccess};
use usbforge_core::{Error, Result};

pub fn enumerator() -> Box<dyn DeviceEnumerator> {
    Box::new(WindowsEnumerator)
}

pub fn disk_access() -> Box<dyn DiskAccess> {
    Box::new(WindowsDiskAccess)
}

struct WindowsEnumerator;

impl DeviceEnumerator for WindowsEnumerator {
    fn list(&self, _only_removable: bool) -> Result<Vec<Device>> {
        Err(Error::unsupported(
            "Windows device enumeration (SetupAPI) not implemented yet",
        ))
    }
}

struct WindowsDiskAccess;

impl DiskAccess for WindowsDiskAccess {
    fn open(&self, _device: &Device, _access: Access) -> Result<Box<dyn BlockDevice>> {
        Err(Error::unsupported(
            "Windows disk access (CreateFile/DeviceIoControl) not implemented yet",
        ))
    }
}
