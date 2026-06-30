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

### GUI toolkit (to confirm)

Candidates, all Rust-native and cross-platform: **Slint** (declarative, closest
to Rufus's form layout, GPLv3-compatible), **iced** (Elm-style), **egui**
(immediate-mode, simplest). Leaning Slint. The GUI is an isolated crate driving
the same core traits, so the choice is low-risk and swappable.

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
- **M1 — Write path (PoC):** open device (`DiskAccess`), raw image write
  (dd-style) with progress + sync + verify; safety guards (refuse system disk).
- **M2 — Partitioning + format:** GPT/MBR via `gpt`/`mbrman`; FAT32/exFAT/ext4.
- **M3 — Bootloaders + ISO:** ISO9660 extract; ms-sys boot records; syslinux,
  GRUB, UEFI:NTFS.
- **M4 — Windows backend:** SetupAPI enumeration + `DeviceIoControl` disk access
  so the core runs on Windows.
- **M5 — Windows UX:** WIM apply, TPM/Secure-Boot bypass via `hivex`, unattend,
  persistence; Fido download; signature checks.
- **M6 — GUI:** device picker, source picker, progress, dark mode, i18n.
- **M7 — Packaging:** deb/rpm/AppImage/Flatpak (Linux), MSI/portable (Windows);
  CI matrix.
