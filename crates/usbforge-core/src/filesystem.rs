//! Filesystem, partition-scheme and boot-target vocabulary.
//!
//! These enums mirror Rufus's `FS_*`, `PARTITION_STYLE_*` and `TT_*` constants
//! and drive both the UI choices and the formatting backend dispatch.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSystem {
    Fat16,
    Fat32,
    ExFat,
    Ntfs,
    Udf,
    Ext2,
    Ext3,
    Ext4,
    /// Windows-only (driver is not available off-Windows); kept for parity.
    Refs,
}

impl FileSystem {
    pub fn label(self) -> &'static str {
        match self {
            FileSystem::Fat16 => "FAT16",
            FileSystem::Fat32 => "FAT32",
            FileSystem::ExFat => "exFAT",
            FileSystem::Ntfs => "NTFS",
            FileSystem::Udf => "UDF",
            FileSystem::Ext2 => "ext2",
            FileSystem::Ext3 => "ext3",
            FileSystem::Ext4 => "ext4",
            FileSystem::Refs => "ReFS",
        }
    }
}

impl fmt::Display for FileSystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Partition table layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionScheme {
    Mbr,
    Gpt,
}

/// Firmware the target machine boots with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSystem {
    Bios,
    Uefi,
    /// BIOS or UEFI-CSM.
    BiosOrUefi,
}

/// What we are putting on the drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMode {
    /// Plain format, no boot code.
    NonBootable,
    /// Write an ISO/disk image (the common case).
    Image,
    /// UEFI:NTFS helper partition (Rufus's signature trick for NTFS UEFI boot).
    UefiNtfs,
}
