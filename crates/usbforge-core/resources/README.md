# Embedded resources

## `uefi-ntfs.img`

A 1 MiB flat FAT image of the **UEFI:NTFS** EFI System Partition, embedded by
`usbforge-core` (see `src/uefi_ntfs.rs`) and written to the small FAT ESP of a
UEFI:NTFS layout so UEFI firmware can boot from an NTFS main partition.

- Source: [pbatard/uefi-ntfs](https://github.com/pbatard/uefi-ntfs) — by Pete
  Batard. This is the exact same binary Rufus ships in `res/uefi/uefi-ntfs.img`.
- Contents: Secure-Boot-signed NTFS UEFI drivers (derived from ntfs-3g),
  exFAT/ARM-NTFS drivers from EfiFs, and the UEFI:NTFS bootloader binaries for
  x64/ia32/arm/aa64/riscv64.
- License: **GPLv3** (same as USBForge). The upstream copyright and license
  notices are preserved; redistribution is under the terms of the GPL.
