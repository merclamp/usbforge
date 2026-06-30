//! Partition-table creation (the equivalent of Rufus's `CreatePartition` /
//! `IOCTL_DISK_SET_DRIVE_LAYOUT_EX`, but written as portable bytes onto a
//! [`BlockDevice`]).
//!
//! Both schemes lay down a **single** data partition spanning (almost) the whole
//! device — the common "format this stick" case. GPT is produced by the `gpt`
//! crate; MBR is a small hand-written table. The returned [`PartitionRegion`] is
//! the byte window the caller then formats.
//!
//! Note: this writes the table *bytes* to the device. On Linux the kernel won't
//! expose a new `/dev/sdX1` node until the partition table is re-read
//! (`BLKRRPART`) — but USBForge formats the partition through a slice of the
//! whole-disk handle, so it does not depend on that node existing.

use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};

use crate::disk::BlockDevice;
use crate::filesystem::{FileSystem, PartitionScheme};
use crate::{Error, Result};

/// Partition alignment / first-partition offset (1 MiB — the modern default).
const ALIGN_BYTES: u64 = 1024 * 1024;
const SECTOR: u64 = 512;

/// A byte window on the device occupied by a partition.
#[derive(Debug, Clone, Copy)]
pub struct PartitionRegion {
    pub start: u64,
    pub len: u64,
}

/// Write a fresh partition table with a single data partition.
pub fn write_single_partition(
    target: &mut dyn BlockDevice,
    scheme: PartitionScheme,
    fs: FileSystem,
    label: &str,
) -> Result<PartitionRegion> {
    match scheme {
        PartitionScheme::Gpt => write_gpt(target, label),
        PartitionScheme::Mbr => write_mbr(target, fs),
    }
}

// ---------------------------------------------------------------------------
// GPT
// ---------------------------------------------------------------------------

fn write_gpt(target: &mut dyn BlockDevice, label: &str) -> Result<PartitionRegion> {
    use gpt::disk::LogicalBlockSize;
    use gpt::{partition_types, GptConfig};

    let dev_size = target.size();
    let total_lba = dev_size / SECTOR;
    // Leave room for both GPT copies + alignment, rounded down to 1 MiB.
    let usable = dev_size.saturating_sub(2 * ALIGN_BYTES) & !(ALIGN_BYTES - 1);
    if usable < ALIGN_BYTES {
        return Err(Error::other("device too small for a GPT partition"));
    }

    // The `gpt` crate writes the GPT header (LBA1) + partition array + backup,
    // but NOT the protective MBR at LBA0. Reborrow `target` so we can write the
    // PMBR ourselves afterwards (a GPT without a valid PMBR is ignored by the
    // Linux kernel, so no /dev/sdX1 would appear).
    let (start, len) = {
        let mut disk = GptConfig::new()
            .writable(true)
            .logical_block_size(LogicalBlockSize::Lb512)
            .create_from_device(GptDev(&mut *target), None)
            .map_err(|e| Error::Other(format!("GPT init failed: {e}")))?;

        let id = disk
            .add_partition(
                label,
                usable,
                partition_types::BASIC,
                0,
                Some(ALIGN_BYTES / SECTOR),
            )
            .map_err(|e| Error::Other(format!("GPT add_partition failed: {e}")))?;

        let part = disk
            .partitions()
            .get(&id)
            .ok_or_else(|| Error::other("created partition not found"))?;
        let start = part
            .bytes_start(LogicalBlockSize::Lb512)
            .map_err(|e| Error::Other(format!("GPT region: {e}")))?;
        let len = part
            .bytes_len(LogicalBlockSize::Lb512)
            .map_err(|e| Error::Other(format!("GPT region: {e}")))?;

        disk.write_inplace()
            .map_err(|e| Error::Other(format!("GPT write failed: {e}")))?;

        (start, len)
    };

    write_protective_mbr(target, total_lba)?;

    Ok(PartitionRegion { start, len })
}

/// Write a GPT protective MBR at LBA0: a single type-`0xEE` partition spanning
/// the whole disk (clamped to 32-bit for disks > 2 TiB), so legacy tooling and
/// the kernel treat the disk as GPT rather than empty/MBR.
fn write_protective_mbr(target: &mut dyn BlockDevice, total_lba: u64) -> Result<()> {
    let size_lba = u32::try_from((total_lba.saturating_sub(1)).min(u32::MAX as u64)).unwrap();

    let mut sector = [0u8; 512];
    let e = 446;
    sector[e] = 0x00; // status: not bootable
    sector[e + 1..e + 4].copy_from_slice(&[0x00, 0x02, 0x00]); // CHS of LBA1
    sector[e + 4] = 0xEE; // GPT protective
    sector[e + 5..e + 8].copy_from_slice(&[0xFF, 0xFF, 0xFF]); // CHS last (max)
    sector[e + 8..e + 12].copy_from_slice(&1u32.to_le_bytes()); // first LBA = 1
    sector[e + 12..e + 16].copy_from_slice(&size_lba.to_le_bytes());
    sector[510] = 0x55;
    sector[511] = 0xAA;

    target.seek(SeekFrom::Start(0))?;
    target.write_all(&sector)?;
    Ok(())
}

/// Adapts `&mut dyn BlockDevice` to what the `gpt` crate wants: an owned
/// `Read + Write + Seek + Debug` value.
struct GptDev<'a>(&'a mut dyn BlockDevice);

impl fmt::Debug for GptDev<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GptDev")
    }
}
impl Read for GptDev<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}
impl Write for GptDev<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
impl Seek for GptDev<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

// ---------------------------------------------------------------------------
// MBR
// ---------------------------------------------------------------------------

fn write_mbr(target: &mut dyn BlockDevice, fs: FileSystem) -> Result<PartitionRegion> {
    let dev_size = target.size();
    let start_lba: u64 = ALIGN_BYTES / SECTOR; // 2048
    let total_lba = dev_size / SECTOR;
    if total_lba <= start_lba + 1 {
        return Err(Error::other("device too small for an MBR partition"));
    }
    // MBR sector counts are 32-bit (caps usable size at ~2 TiB — fine for UFDs).
    let count_lba = u32::try_from((total_lba - start_lba).min(u32::MAX as u64)).unwrap();

    let type_byte = mbr_type_byte(fs);

    let mut sector = [0u8; 512];
    let e = 446; // first partition entry
    sector[e] = 0x00; // status: not active
    sector[e + 1..e + 4].copy_from_slice(&[0xFE, 0xFF, 0xFF]); // CHS first (LBA placeholder)
    sector[e + 4] = type_byte;
    sector[e + 5..e + 8].copy_from_slice(&[0xFE, 0xFF, 0xFF]); // CHS last (LBA placeholder)
    sector[e + 8..e + 12].copy_from_slice(&(start_lba as u32).to_le_bytes());
    sector[e + 12..e + 16].copy_from_slice(&count_lba.to_le_bytes());
    sector[510] = 0x55;
    sector[511] = 0xAA;

    target.seek(SeekFrom::Start(0))?;
    target.write_all(&sector)?;

    Ok(PartitionRegion {
        start: start_lba * SECTOR,
        len: u64::from(count_lba) * SECTOR,
    })
}

/// MBR partition type byte for a filesystem.
fn mbr_type_byte(fs: FileSystem) -> u8 {
    match fs {
        FileSystem::Fat16 => 0x0E, // FAT16 LBA
        FileSystem::Fat32 => 0x0C, // FAT32 LBA
        FileSystem::Ntfs | FileSystem::ExFat => 0x07,
        FileSystem::Ext2 | FileSystem::Ext3 | FileSystem::Ext4 => 0x83,
        FileSystem::Udf | FileSystem::Refs => 0x07,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::{FileSystem, PartitionScheme};
    use crate::testutil::MemDevice;

    #[test]
    fn gpt_single_partition_roundtrip() {
        let mut dev = MemDevice::new(256 * 1024 * 1024);
        let region = write_single_partition(
            &mut dev,
            PartitionScheme::Gpt,
            FileSystem::Fat32,
            "USBFORGE",
        )
        .unwrap();
        assert!(region.start >= ALIGN_BYTES);
        assert!(region.len > 0);

        // Protective MBR present at LBA0 (0xEE partition + boot signature) —
        // without it the Linux kernel ignores the GPT.
        assert_eq!(dev.data()[446 + 4], 0xEE);
        assert_eq!(dev.data()[510], 0x55);
        assert_eq!(dev.data()[511], 0xAA);

        // Re-open with the gpt crate and confirm exactly one partition.
        use gpt::disk::LogicalBlockSize;
        use gpt::GptConfig;
        let disk = GptConfig::new()
            .writable(false)
            .logical_block_size(LogicalBlockSize::Lb512)
            .open_from_device(GptDev(&mut dev))
            .unwrap();
        assert_eq!(disk.partitions().len(), 1);
    }

    #[test]
    fn mbr_single_partition_layout() {
        let mut dev = MemDevice::new(64 * 1024 * 1024);
        let region =
            write_single_partition(&mut dev, PartitionScheme::Mbr, FileSystem::Fat32, "X").unwrap();
        assert_eq!(region.start, 2048 * 512);
        let b = dev.data();
        assert_eq!(b[510], 0x55);
        assert_eq!(b[511], 0xAA);
        assert_eq!(b[446 + 4], 0x0C); // FAT32 LBA type byte
                                      // First-LBA field == 2048.
        assert_eq!(u32::from_le_bytes(b[454..458].try_into().unwrap()), 2048);
    }
}
