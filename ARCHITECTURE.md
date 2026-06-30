# USBForge — Architecture & Roadmap

This document captures the design that came out of a module-by-module analysis
of Rufus, and the plan for reimplementing its functionality in Rust as a
cross-platform (Linux + Windows) application.

## 1. Principles

1. **Clean core, no globals.** Rufus couples worker threads to the GUI via three
   global side-channels: `uprintf()` (log), `UpdateProgress()` (progress bar),
   and shared state (`SelectedDrive`, `boot_type`, `image_path`). USBForge
   replaces all three with explicit seams: a `Reporter` trait and value types
   passed as parameters. The core has **no** OS or GUI dependency.
2. **Traits instead of a C HAL.** Rufus's `drive.h` / `dev.h` are an informal
   abstraction boundary. We formalise it as traits (`DeviceEnumerator`,
   `DiskAccess`, `BlockDevice`) implemented per-OS.
3. **`cfg(target_os)` instead of `#ifdef`.** One backend module per OS, selected
   at compile time.
4. **Reuse algorithms, rewrite platform glue.** The portable, algorithmic parts
   of Rufus (FAT layout math, ms-sys boot records) and its bundled libraries
   (e2fsprogs, libcdio, wimlib) have living cross-platform upstreams. Prefer a
   maintained Rust crate; fall back to FFI against the upstream C library;
   reimplement only when neither exists.

> **Working in parallel on Linux and Windows?** The trait boundary in
> `usbforge-core` is the contract both backends implement. See
> [`docs/WORK-SPLIT.md`](docs/WORK-SPLIT.md) for ownership and the per-milestone
> Linux/Windows split.

## 2. Layering

```
              ┌───────────────┐      ┌───────────────┐
              │ usbforge-cli  │      │ usbforge-gui  │   frontends
              └───────┬───────┘      └───────┬───────┘
                      │   Reporter / value types     │
                      └───────────┬──────────────────┘
                          ┌───────▼────────┐
                          │ usbforge-core  │  domain + traits (HAL) + algorithms
                          └───────┬────────┘
                          ┌───────▼────────────┐
                          │ usbforge-platform  │  linux / windows backends
                          └───────┬────────────┘
                         POSIX / libudev / WinAPI
```

## 3. Rufus → USBForge mapping

| Rufus area | Files | USBForge home | Approach |
|------------|-------|---------------|----------|
| Device enumeration | `dev.c` (SetupAPI, CfgMgr) | `platform/{linux,windows}` impl of `DeviceEnumerator` | Linux: `/sys/block` (done) → `libudev` for hotplug; Windows: SetupAPI |
| Disk I/O + partitions | `drive.c` (`DeviceIoControl`, VDS COM) | `platform` impl of `DiskAccess`/`BlockDevice` + `core::partition` | Linux: `ioctl(BLK*)` + `gpt`/`mbrman`; Windows: `DeviceIoControl` |
| Format orchestration | `format.c` (`fmifs.dll`, VDS) | `core::format` (planned) | Drive `mkfs.*` / fs crates; not COM |
| FAT32 (large) | `format_fat32.c` | `core::fs::fat` | `fatfs` crate or port the (portable) layout math |
| ext2/3/4 | `format_ext.c`, `ext2fs/` (e2fsprogs fork) | `core::fs::ext` | shell `mkfs.ext4` or FFI e2fsprogs |
| Boot records | `ms-sys/` (originally a Linux tool) | `core::boot` | reimplement writers (small, GPLv3) over `BlockDevice` |
| Syslinux/GRUB | `syslinux/`, `res/grub*` | `core::boot::loaders` | embed payloads, install over `BlockDevice` |
| ISO read/extract | `iso.c`, `libcdio/` | `core::image::iso` | `cdfs` crate (ISO9660) + UDF; I/O via std |
| Decompression | `bled/` (busybox fork) | `core::image::compressed` | `flate2`, `xz2`, `zstd`, `bzip2` crates |
| WIM (To Go / WUE) | `wimlib/` | `core::image::wim` | FFI to upstream `wimlib` |
| Hashing | `hash.c` | `core::hash` (done) | RustCrypto: `sha2`, `sha1`, `md-5` |
| Windows UX / TPM bypass | `wue.c` | `core::wue` (planned) | edit offline WIM + registry hives via `hivex`/FFI |
| ISO download (Fido) | `net.c` + Fido.ps1 | `core::net` (planned) | `reqwest`/`ureq`, reimplement Fido logic (no PowerShell) |
| Signature checks | `pki.c` (CryptoAPI) | `core::pki` (planned) | `rsa`/`sha2` or `openssl` crate; `osslsigncode` for Authenticode |
| Settings | registry / `settings.h` | `core::settings` (planned) | TOML at XDG / `%APPDATA%` |
| Locked-file detection | `process.c` (NtQuery…) | `platform` | Linux: `/proc`; Windows: RM API |
| Localization | `.loc` (38 langs) | GUI | convert to Fluent / Qt-Linguist-style catalogs |
| GUI | `rufus.c`, `ui.c`, `stdlg.c`, `.rc` | `usbforge-gui` | rewrite (toolkit below) |

## 4. Dependency choices (Rust crates)

- **Enumeration / hotplug:** `udev` (Linux), `windows` (Win32).
- **Low-level device ops:** `nix`/`rustix` (ioctls, Linux), `windows` (Win32).
- **Partition tables:** `gpt`, `mbrman`.
- **Filesystems:** `fatfs` (FAT), FFI/`mkfs` for ext4/exFAT/NTFS.
- **Images:** `cdfs` (ISO9660), `flate2`/`xz2`/`zstd`/`bzip2` (compression),
  FFI `wimlib` (WIM).
- **Registry hives (offline):** `hivex` (FFI) for the Win11 TPM-bypass edits.
- **Hashing:** `sha2`, `sha1`, `md-5`, `hex` (in use).
- **Networking:** `reqwest` or `ureq`.
- **Crypto/signatures:** `rsa` + `sha2`, or `openssl`.
- **CLI:** `clap` (in use). **Errors:** `thiserror` (lib), `anyhow` (bin).

### GUI toolkit — **Slint (chosen)**

Slint (declarative `.slint` UI compiled by `slint-build`, GPLv3-compatible) is
the closest Rust-native toolkit to Rufus's form layout. The `usbforge-gui` crate
drives the same core traits the CLI uses; long operations run on a worker thread
and report progress/log to the UI via `slint::invoke_from_event_loop`. Native
file picker via `rfd`. A working window (device dropdown, write/format/create,
progress + log) exists and renders on Wayland/X11.

## 5. Feature parity & hard blockers

Achievable natively (~88% of Rufus): ISO→USB (Windows & Linux), FAT32 / NTFS /
exFAT / UDF / ext2-4, BIOS+UEFI boot, syslinux/GRUB/UEFI:NTFS, **Win11 TPM /
Secure Boot bypass** (offline hive edit via `hivex`), Linux persistence,
checksums, fake-flash detection, locked-file detection, ISO download (Fido logic
reimplemented), signature verification, VHD/VHDX images (`qemu-img`/`libvhdi`).

**Hard blockers (depend on closed Microsoft components — out of scope / stubbed):**

- **Windows To Go** — needs `bcdboot.exe`/`bcdedit.exe`; BCD is a closed format
  with no Linux equivalent. Reverse-engineering it is a separate project.
- **FFU image creation** — `DISM`/wimgapi, Windows-only format.
- **ReFS formatting** — Windows-only driver.

These surface in the UI as disabled/explained options rather than silent gaps.

## 6. Roadmap

- **M0 — Scaffold (done):** workspace, core traits, Linux sysfs enumeration,
  hashing, CLI `list`/`hash`/`inspect`.
- **M1 — Write path (done, PoC):** raw image write (dd-style) with progress +
  flush/sync + read-back verify (`core::write`); device open via `O_EXCL`
  (`platform::linux::block`); CLI `write` with safety guards (device must be
  enumerated; refuse fixed/system disk without `--allow-fixed`, refuse
  write-protected, size check, typed confirmation). _Still TODO: `O_DIRECT` +
  aligned I/O for throughput, `BLKRRPART`/volume locking, compressed sources._
- **M2 — Partitioning + format:** GPT/MBR via `gpt`/`mbrman`; FAT32/exFAT/ext4.
- **M3 — ISO + UEFI file-copy (done):** ISO9660 read/scan via `cdfs`
  (`core::iso`: volume label, file/byte totals, UEFI-arch + Windows-installer +
  BIOS-loader detection); recursive extraction into a FAT32 volume via `fatfs`;
  CLI `create` = GPT(+PMBR)/MBR with an **EFI System Partition** + FAT32 + ISO
  extraction → UEFI-bootable media. Verified on hardware with a real Alpine ISO
  (114 files, bootable ESP).
  **BIOS boot via isohybrid (done):** `core::iso::is_isohybrid` detects ISOs that
  carry a real MBR boot sector; `inspect`/`create` surface it and `write` notes
  it. For such ISOs (Alpine and most Linux distros) a raw `write` boots on BIOS
  *and* UEFI using the ISO's own tested loader — verified on hardware (isolinux
  MBR + El Torito ESP).
  **UEFI:NTFS (done):** `create --fs ntfs|auto` writes a two-partition layout
  (`layout::write_uefi_ntfs_layout`: NTFS main + a 1 MiB FAT ESP holding the
  embedded UEFI:NTFS bootloader, `core::uefi_ntfs`), formats the main partition
  with host `mkfs.ntfs`, mounts it and copies the ISO tree in
  (`iso::extract_to_dir`). Lets UEFI boot from media whose files exceed FAT32's
  4 GiB limit (Windows ISOs). Verified on hardware (NTFS main + UEFI:NTFS ESP
  with x64/aa64/… bootloaders). _Still TODO: a pure-Rust syslinux/GRUB installer
  for non-isohybrid BIOS boot; UDF reading for real Windows ISOs (cdfs is
  ISO9660-only); NTFS format/copy currently uses host ntfs-3g (Linux)._
- **M4 — Windows backend:** SetupAPI enumeration + `DeviceIoControl` disk access
  so the core runs on Windows.
- **M5 — Windows UX:** WIM apply, TPM/Secure-Boot bypass via `hivex`, unattend,
  persistence; Fido download; signature checks.
- **M6 — GUI (v1 done):** Slint window — device dropdown + refresh, image
  picker (`rfd`), write/format/create modes, scheme/label fields, erase
  confirmation, progress bar + log; worker-thread execution with a `GuiReporter`.
  Verified rendering on Wayland with a real device listed. _TODO: in-GUI
  privilege elevation (pkexec/UAC), dark mode polish, i18n, cancel button._
- **M7 — Packaging:** deb/rpm/AppImage/Flatpak (Linux), MSI/portable (Windows);
  CI matrix.
