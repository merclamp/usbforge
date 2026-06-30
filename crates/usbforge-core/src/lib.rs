//! # usbforge-core
//!
//! Platform-agnostic heart of USBForge. This crate contains **no** OS-specific
//! code and **no** GUI code. It defines the domain model (devices, partitions,
//! filesystems, images), the abstraction traits that platform backends
//! implement (the equivalent of Rufus's `drive.h` / `dev.h` "HAL"), and pure
//! algorithms (hashing today; FAT/boot-record writers later).
//!
//! Frontends (CLI, GUI) talk to the core through these traits plus a
//! [`Reporter`] for logging and progress — replacing Rufus's global
//! `uprintf()` / `UpdateProgress()` / `SelectedDrive` glue with explicit,
//! testable seams.
//!
//! ## Layering
//! ```text
//!   frontend (cli/gui)  ->  usbforge-core (traits + domain)  <-  usbforge-platform (linux/windows)
//! ```

pub mod device;
pub mod disk;
pub mod error;
pub mod filesystem;
pub mod format;
pub mod hash;
pub mod image;
pub mod iso;
pub mod layout;
pub mod report;
pub mod write;

#[cfg(test)]
mod testutil;

pub use error::{Error, Result};
pub use report::{Level, NullReporter, Reporter};

/// Product name, surfaced by frontends.
pub const PRODUCT: &str = "USBForge";
