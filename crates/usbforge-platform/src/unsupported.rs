//! Fallback backend for targets without a real implementation (macOS, *BSD).
//! Keeps the workspace compiling everywhere; every call returns `Unsupported`.

use usbforge_core::device::{Device, DeviceEnumerator};
use usbforge_core::disk::{Access, BlockDevice, DiskAccess};
use usbforge_core::{Error, Result};

pub fn enumerator() -> Box<dyn DeviceEnumerator> {
    Box::new(UnsupportedBackend)
}

pub fn disk_access() -> Box<dyn DiskAccess> {
    Box::new(UnsupportedBackend)
}

struct UnsupportedBackend;

impl DeviceEnumerator for UnsupportedBackend {
    fn list(&self, _only_removable: bool) -> Result<Vec<Device>> {
        Err(Error::unsupported("no backend for this operating system"))
    }
}

impl DiskAccess for UnsupportedBackend {
    fn open(&self, _device: &Device, _access: Access) -> Result<Box<dyn BlockDevice>> {
        Err(Error::unsupported("no backend for this operating system"))
    }
}
