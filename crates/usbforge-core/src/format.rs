//! Filesystem creation (Rufus's `FormatPartition` family).
//!
//! M2 implements **FAT32** with the pure-Rust `fatfs` crate — identical on Linux
//! and Windows, no external `mkfs`. exFAT / ext4 / NTFS need external tooling
//! (`mkfs.exfat`, e2fsprogs, ntfs-3g) and arrive as platform-assisted
//! formatters in a later milestone.
//!
//! [`PartitionSlice`] bounds reads/writes/seeks to a `[start, start+len)` window
//! of a whole-disk [`BlockDevice`], so we can format a partition through the
//! whole-disk handle without depending on a kernel partition node.

use std::io::{self, Read, Seek, SeekFrom, Write};

use fatfs::{format_volume, FatType, FormatVolumeOptions};

use crate::disk::BlockDevice;
use crate::{Error, Result};

/// A bounded read/write/seek view over a region of a block device.
pub struct PartitionSlice<'a> {
    dev: &'a mut dyn BlockDevice,
    start: u64,
    len: u64,
    pos: u64,
}

impl<'a> PartitionSlice<'a> {
    pub fn new(dev: &'a mut dyn BlockDevice, start: u64, len: u64) -> Self {
        PartitionSlice {
            dev,
            start,
            len,
            pos: 0,
        }
    }
}

impl Read for PartitionSlice<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.len - self.pos;
        if remaining == 0 {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(remaining) as usize;
        self.dev.seek(SeekFrom::Start(self.start + self.pos))?;
        let read = self.dev.read(&mut buf[..n])?;
        self.pos += read as u64;
        Ok(read)
    }
}

impl Write for PartitionSlice<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let remaining = self.len - self.pos;
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "write past end of partition",
            ));
        }
        let n = (buf.len() as u64).min(remaining) as usize;
        self.dev.seek(SeekFrom::Start(self.start + self.pos))?;
        let written = self.dev.write(&buf[..n])?;
        self.pos += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.dev.flush()
    }
}

impl Seek for PartitionSlice<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(o) => o as i64,
            SeekFrom::End(o) => self.len as i64 + o,
            SeekFrom::Current(o) => self.pos as i64 + o,
        };
        if target < 0 || target as u64 > self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek out of partition bounds",
            ));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

/// Format the given stream (a whole partition) as FAT32 with `label`.
pub fn format_fat32(stream: &mut (impl Read + Write + Seek), label: &str) -> Result<()> {
    stream.seek(SeekFrom::Start(0))?;
    let options = FormatVolumeOptions::new()
        .fat_type(FatType::Fat32)
        .volume_label(label_bytes(label));
    format_volume(stream, options)
        .map_err(|e| Error::Other(format!("FAT32 format failed: {e}")))?;
    Ok(())
}

/// FAT volume labels are exactly 11 bytes, space-padded, conventionally
/// upper-case ASCII.
fn label_bytes(label: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    for (slot, byte) in out.iter_mut().zip(label.bytes().take(11)) {
        *slot = byte.to_ascii_uppercase();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::MemDevice;

    #[test]
    fn fat32_format_then_mount() {
        let mut dev = MemDevice::new(128 * 1024 * 1024);

        let len = dev.size();
        {
            let mut slice = PartitionSlice::new(&mut dev, 0, len);
            format_fat32(&mut slice, "TESTVOL").unwrap();
        }

        // Mount it back with fatfs and check it really is FAT32 with our label.
        let len = dev.size();
        let mut slice = PartitionSlice::new(&mut dev, 0, len);
        let fs = fatfs::FileSystem::new(&mut slice, fatfs::FsOptions::new()).unwrap();
        assert_eq!(fs.fat_type(), FatType::Fat32);
        assert_eq!(fs.volume_label().trim(), "TESTVOL");

        // And we can create a file in the root dir.
        let root = fs.root_dir();
        let mut f = root.create_file("HELLO.TXT").unwrap();
        f.write_all(b"usbforge").unwrap();
    }

    #[test]
    fn slice_bounds_are_enforced() {
        let mut dev = MemDevice::new(1024 * 1024);
        let mut slice = PartitionSlice::new(&mut dev, 4096, 8192);
        // Seeking past the window is rejected.
        assert!(slice.seek(SeekFrom::Start(9000)).is_err());
        // Seeking to the end is allowed and reports the window length.
        assert_eq!(slice.seek(SeekFrom::End(0)).unwrap(), 8192);
    }
}
