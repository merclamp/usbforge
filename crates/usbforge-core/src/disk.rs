//! Raw block-device access traits.
//!
//! This is the write-path counterpart to [`crate::device`]. Rufus opens
//! `\\.\PhysicalDriveN` with `CreateFile` and drives it with `DeviceIoControl`
//! (`IOCTL_DISK_*`, `FSCTL_*`). The portable shape of that is: get a seekable
//! byte stream over the whole device, plus a few capability calls (sector size,
//! flush, lock/unmount). Platform backends provide the implementation
//! (`open()` + `ioctl(BLK*)` on Linux; `CreateFile` + `DeviceIoControl` on
//! Windows).

use std::io::{Read, Seek, Write};

/// A whole-disk byte stream. The blanket bound on the std I/O traits lets the
/// portable algorithms (image writer, boot-record writer, FAT formatter) treat
/// a device exactly like a file.
pub trait BlockDevice: Read + Write + Seek + Send {
    /// Logical sector size in bytes.
    fn sector_size(&self) -> u32;

    /// Total addressable size in bytes.
    fn size(&self) -> u64;

    /// Flush OS buffers down to the medium (Linux `fsync`/`BLKFLSBUF`,
    /// Windows `FlushFileBuffers`).
    fn sync(&mut self) -> crate::Result<()>;
}

/// How a device should be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    /// Exclusive read/write: unmount/lock partitions first so nothing else
    /// writes underneath us (Rufus `UnmountVolume` + `FSCTL_LOCK_VOLUME`).
    ReadWriteExclusive,
}

/// Opens [`BlockDevice`] handles for a [`crate::device::Device`]. Implemented
/// per-platform.
pub trait DiskAccess {
    fn open(
        &self,
        device: &crate::device::Device,
        access: Access,
    ) -> crate::Result<Box<dyn BlockDevice>>;
}
