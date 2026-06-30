//! Raw block-device access on Linux.
//!
//! Opening `/dev/<name>` and treating it as a seekable file already gives us
//! read/write/seek. The capability methods are intentionally conservative for
//! now: `sync()` does an `fsync`, and `sector_size`/`size` are filled in at open
//! time from sysfs-derived values passed by the caller. `ioctl(BLKRRPART)`,
//! `BLKFLSBUF`, `O_DIRECT` and volume locking arrive with the formatting
//! milestone.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;

use usbforge_core::device::Device;
use usbforge_core::disk::{Access, BlockDevice, DiskAccess};
use usbforge_core::{Error, Result};

pub struct LinuxDiskAccess;

impl DiskAccess for LinuxDiskAccess {
    fn open(&self, device: &Device, access: Access) -> Result<Box<dyn BlockDevice>> {
        if matches!(access, Access::ReadWriteExclusive) && device.read_only {
            return Err(Error::Refused(format!(
                "{} is write-protected",
                device.path
            )));
        }

        let mut opts = OpenOptions::new();
        match access {
            Access::ReadOnly => {
                opts.read(true);
            }
            Access::ReadWriteExclusive => {
                // O_EXCL on a block device asks the kernel for exclusive access
                // (fails if a filesystem is mounted), which is the behaviour we
                // want before clobbering a disk.
                opts.read(true).write(true).custom_flags(libc_o_excl());
            }
        }

        let file = opts
            .open(&device.path)
            .map_err(|e| Error::Device(format!("cannot open {}: {e}", device.path)))?;

        Ok(Box::new(LinuxBlockDevice {
            file,
            sector_size: device.logical_sector_size.max(512),
            size: device.size,
        }))
    }
}

/// `O_EXCL` without pulling in the `libc` crate yet. On a block device this
/// requests an exclusive open (fails if the disk is mounted / held). Value is
/// stable in the Linux ABI across architectures (`0o200`). To be replaced with
/// `libc::O_EXCL` / `rustix` when we add ioctls in M2.
///
/// (Note: `0o200000` is `O_DIRECTORY`, not `O_EXCL` — getting this wrong makes
/// `open()` fail with `ENOTDIR` on a device node.)
fn libc_o_excl() -> i32 {
    0o200
}

struct LinuxBlockDevice {
    file: File,
    sector_size: u32,
    size: u64,
}

impl Read for LinuxBlockDevice {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl Write for LinuxBlockDevice {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Seek for LinuxBlockDevice {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(pos)
    }
}

impl BlockDevice for LinuxBlockDevice {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }
    fn size(&self) -> u64 {
        self.size
    }
    fn sync(&mut self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }
}
