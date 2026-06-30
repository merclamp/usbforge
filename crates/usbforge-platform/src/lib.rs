//! # usbforge-platform
//!
//! OS-specific backends that implement the `usbforge-core` traits. The right
//! module is selected at compile time with `cfg(target_os)` — the Rust
//! equivalent of Rufus's `#ifdef _WIN32`. Frontends should not name a backend
//! directly; they call [`device_enumerator`] / [`disk_access`].

use usbforge_core::device::DeviceEnumerator;
use usbforge_core::disk::DiskAccess;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as backend;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as backend;

// Fallback so the crate still type-checks on other targets (macOS, BSD, ...).
#[cfg(not(any(target_os = "linux", windows)))]
mod unsupported;
#[cfg(not(any(target_os = "linux", windows)))]
use unsupported as backend;

/// The device enumerator for the current OS.
pub fn device_enumerator() -> Box<dyn DeviceEnumerator> {
    backend::enumerator()
}

/// The raw disk-access provider for the current OS.
pub fn disk_access() -> Box<dyn DiskAccess> {
    backend::disk_access()
}
