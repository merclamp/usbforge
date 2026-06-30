//! UEFI:NTFS bridge bootloader.
//!
//! UEFI firmware can only boot from FAT, but Windows install ISOs carry an
//! `install.wim` larger than FAT32's 4 GiB file limit. Rufus's solution is a
//! tiny extra FAT EFI System Partition holding the **UEFI:NTFS** bootloader,
//! which loads an NTFS UEFI driver and chainloads the Windows loader from the
//! main NTFS partition.
//!
//! The 1 MiB flat FAT image embedded here is the prebuilt UEFI:NTFS partition
//! from the [uefi-ntfs](https://github.com/pbatard/uefi-ntfs) project (GPLv3,
//! by Pete Batard) — the same binary Rufus ships in `res/uefi/uefi-ntfs.img`.
//! It contains Secure-Boot-signed NTFS/exFAT UEFI drivers + the UEFI:NTFS
//! bootloader for x64/ia32/arm/aa64.

use std::io::SeekFrom;

use crate::disk::BlockDevice;
use crate::Result;

/// The flat FAT image of the UEFI:NTFS EFI System Partition (exactly 1 MiB).
pub const UEFI_NTFS_IMG: &[u8] = include_bytes!("../resources/uefi-ntfs.img");

/// Size the UEFI:NTFS ESP must be to hold the image.
pub const ESP_SIZE: u64 = UEFI_NTFS_IMG.len() as u64;

/// Write the UEFI:NTFS bootloader image into the ESP region at `esp_start`
/// (a byte offset on the whole-disk handle).
pub fn write_esp(target: &mut dyn BlockDevice, esp_start: u64) -> Result<()> {
    target.seek(SeekFrom::Start(esp_start))?;
    target.write_all(UEFI_NTFS_IMG)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_is_one_mib_fat() {
        assert_eq!(UEFI_NTFS_IMG.len(), 1024 * 1024);
        // FAT boot sector signature at the end of sector 0.
        assert_eq!(UEFI_NTFS_IMG[510], 0x55);
        assert_eq!(UEFI_NTFS_IMG[511], 0xAA);
    }
}
