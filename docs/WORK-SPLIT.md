# Work Split — Linux vs Windows (keeping it cross-platform)

This project is built by two people in parallel:

- **Linux side** — owns the Linux backend, develops/tests on Linux.
- **Windows side** — owns the Windows backend, develops/tests on Windows.

Most of the codebase, however, is **shared** and OS-neutral. This document
defines who owns what, and the rules that let both people work at the same time
without conflicts.

## The golden rule

> **OS-specific code lives _only_ in `usbforge-platform`, behind
> `cfg(target_os)`. Everything else is shared and must compile identically on
> both operating systems.**

Concretely:

- `usbforge-core` and the frontends (`usbforge-cli`, `usbforge-gui`) contain
  **no** `#[cfg(target_os)]`, **no** Win32/POSIX calls, and **no** path
  assumptions (`\\.\PhysicalDrive…` / `/dev/sd…`). They are the same on both.
- Linux code → `usbforge-platform/src/linux/`. Windows code →
  `usbforge-platform/src/windows/`. The two never call into each other.
- The **contract** between shared code and the backends is the set of **traits**
  in `usbforge-core`:
  - `device::DeviceEnumerator` — list attached devices.
  - `disk::DiskAccess` + `disk::BlockDevice` — open and read/write a device.
  - *(coming)* `Partitioner`, `Formatter`, `BootInstaller`.
  Both backends implement the **same** traits. As long as the trait signatures
  don't change, the two backends evolve independently.

### The only thing that needs coordination

Changing a **trait signature in `usbforge-core`** affects both backends. Treat
it like a public API change: agree on it first, then update `linux/` and
`windows/` together (ideally in the same PR). Day-to-day work inside your own
backend module needs no coordination.

## Ownership map

| Layer | Path | Owner | OS-specific? |
|-------|------|-------|--------------|
| Domain types, traits (the contract) | `usbforge-core` | **Shared** (coordinate changes) | No |
| Pure algorithms (hash, write engine, FAT/ext layout, boot records, ISO parse, decompress, partition tables…) | `usbforge-core` | **Shared** (either person) | No |
| Reporter (log/progress) | `usbforge-core` | **Shared** | No |
| CLI frontend | `usbforge-cli` | **Shared** | No |
| GUI frontend (one Rust toolkit, builds on both) | `usbforge-gui` | **Shared** | No |
| Device enumeration / disk I/O / mounting | `usbforge-platform/src/linux` | **Linux side** | **Yes** |
| Device enumeration / disk I/O / mounting | `usbforge-platform/src/windows` | **Windows side** | **Yes** |
| Backend selection (`cfg`) | `usbforge-platform/src/lib.rs` | **Shared** | the cfg itself |
| Packaging (deb/rpm/AppImage/Flatpak) | `packaging/linux` *(tbd)* | **Linux side** | Yes |
| Packaging (MSI/portable, code signing) | `packaging/windows` *(tbd)* | **Windows side** | Yes |

## A subtlety: "Windows _features_" are mostly _shared_ code

Producing **Windows install media** (TPM/Secure-Boot bypass, unattend.xml,
OneDrive/Copilot removal, WIM apply) is a Rufus feature set — but the code that
does it is **filesystem and registry-hive manipulation that runs on either host
OS**. Editing an offline registry hive inside `install.wim` uses `hivex`
(cross-platform), not the live Windows registry. So:

- **"Windows backend"** = making USBForge _run_ on Windows (enumerate devices,
  open `\\.\PhysicalDrive…`). → **Windows side**, in `platform/windows`.
- **"Windows-target features"** = building Windows media. → **Shared engine** in
  `usbforge-core` (works when the app runs on Linux too).

The genuinely host-Windows-only items (no Linux equivalent) are the
[hard blockers](../ARCHITECTURE.md#5-feature-parity--hard-blockers): **Windows
To Go** (`bcdboot`/BCD) and **FFU** (DISM). Those are out of scope / stubbed.

## Per-milestone split

Roadmap milestones are in [`ARCHITECTURE.md`](../ARCHITECTURE.md#6-roadmap).
Here is the same work, sliced by owner. "Shared" items can be done by either
person and benefit both OSes immediately.

| Milestone | Shared engine (`core`/frontends) | Linux side (`platform/linux`) | Windows side (`platform/windows`) |
|-----------|----------------------------------|-------------------------------|-----------------------------------|
| **M0/M1 — done (Linux)** | types, traits, hashing, raw write+verify, CLI | sysfs enum, `O_EXCL` open, block R/W | **TODO: reach M0/M1 parity** (see quick-start below) |
| **M2 — partition + format** | GPT/MBR table builders (`gpt`/`mbrman`), FAT32/exFAT/ext4 formatters | apply layout via `ioctl(BLKRRPART/BLKPG)`, `BLKDISCARD`, unmount before write | apply layout via `IOCTL_DISK_SET_DRIVE_LAYOUT_EX` + `IOCTL_DISK_UPDATE_PROPERTIES`, `FSCTL_DISMOUNT_VOLUME` |
| **M3 — bootloaders + ISO** | ISO9660 extract, ms-sys boot records, syslinux/GRUB/UEFI:NTFS install over `BlockDevice` | — (mostly shared; verify on real HW) | — (mostly shared; verify on real HW) |
| **M4 — Windows backend** | — | — | **Bulk of Windows work:** robust SetupAPI enum, full `DeviceIoControl` disk ops, volume locking |
| **M5 — Windows-target UX** | WIM apply (`wimlib` FFI), TPM/SB bypass (`hivex`), unattend, persistence, Fido download, signature checks | — | invoke `bcdboot` for the (stubbed) To-Go path only |
| **M6 — GUI** | device picker, source picker, progress, dark mode, i18n (one shared Rust UI) | window/theming smoke-test on Linux | window/theming smoke-test on Windows |
| **M7 — packaging + CI** | release metadata | deb/rpm/AppImage/Flatpak, `udev`/polkit | MSI/portable, UAC manifest, code signing |

## Windows quick-start — reach parity with the current Linux build

The Windows backend is currently a stub
(`platform/src/windows/mod.rs`) that returns `Unsupported`. Two tasks bring it
to where Linux is today (`list` + `write` working):

1. **Add the dependency** (gated so it never affects the Linux build), in
   `crates/usbforge-platform/Cargo.toml`:
   ```toml
   [target.'cfg(windows)'.dependencies]
   windows = { version = "0.58", features = [
     "Win32_Foundation",
     "Win32_Storage_FileSystem",
     "Win32_System_Ioctl",
     "Win32_System_IO",
     "Win32_Devices_DeviceAndDriverInstallation", # SetupAPI / CfgMgr
   ] }
   ```

2. **Implement `WindowsEnumerator::list`** — enumerate physical disks and fill
   `core::device::Device`. Two viable routes (pick one):
   - SetupAPI + CfgMgr: `SetupDiGetClassDevs(GUID_DEVINTERFACE_DISK)`,
     `SetupDiEnumDeviceInterfaces`, `SetupDiGetDeviceInterfaceDetail`, then
     `CM_Get_Device_ID` / registry props for model/bus/removable. (This is what
     Rufus `dev.c` does.)
   - Simpler first cut: open each `\\.\PhysicalDriveN`, query
     `IOCTL_STORAGE_QUERY_PROPERTY` (`StorageDeviceProperty` →
     `STORAGE_DEVICE_DESCRIPTOR` for bus type / removable) and
     `IOCTL_DISK_GET_LENGTH_INFO` (size). Loop N until open fails.
   Map bus → `Bus::Usb/Sata/Nvme/…`; set `path = \\.\PhysicalDriveN`,
   `id = PhysicalDriveN`, `removable`, `read_only`.

3. **Implement `WindowsDiskAccess::open`** → a `WindowsBlockDevice`:
   - `CreateFileW(\\.\PhysicalDriveN, GENERIC_READ|GENERIC_WRITE,
     FILE_SHARE_READ|FILE_SHARE_WRITE, …, OPEN_EXISTING, 0, …)`.
   - For exclusive write: `FSCTL_LOCK_VOLUME` (+ `FSCTL_DISMOUNT_VOLUME`) on the
     volume handles, and `FSCTL_ALLOW_EXTENDED_DASD_IO`.
   - Implement `Read`/`Write`/`Seek` over the handle (`ReadFile`/`WriteFile`/
     `SetFilePointerEx`), `sync()` → `FlushFileBuffers`, `size()` from
     `IOCTL_DISK_GET_LENGTH_INFO`, `sector_size()` from
     `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`.

After that, `usbforge list` and `usbforge write` work on Windows — full parity
with the Linux PoC, using the **exact same** `core::write` engine and CLI.

> Tip: writing to a physical drive on Windows requires running the terminal
> **as Administrator** (the equivalent of `sudo` on Linux).

## Linux side — remaining backend tasks

- `libudev` for hotplug add/remove events (today's `/sys/block` scan is
  one-shot); keep the sysfs path as a no-dep fallback.
- ioctls via `rustix`/`nix`: `BLKGETSIZE64`, `BLKSSZGET` (sector size),
  `BLKRRPART` (re-read partition table after writing), `BLKFLSBUF`,
  `BLKDISCARD` (fast wipe), `O_DIRECT` + aligned buffers for throughput.
- Unmount busy partitions before an exclusive open (`umount2`), and
  locked-file detection via `/proc/*/fd`.
- Privilege story: polkit action or `pkexec`, and packaging (deb/rpm/AppImage/
  Flatpak) + `udev` rules.

## Coordination workflow

- **Branches/PRs:** small PRs per backend. Anything touching `usbforge-core`
  traits gets a quick heads-up to the other person.
- **Don't break the other OS:** never put `#[cfg(windows)]`/`#[cfg(unix)]` in
  `core` or the frontends. If you feel the need to, the abstraction is wrong —
  push it down into a trait instead.
- **CI builds both targets** on every push (see
  [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)): `cargo build` +
  `cargo test` on `ubuntu-latest` and `windows-latest`. Shared `core` tests run
  on both; a red Windows job means the contract or a shared change leaked an
  OS assumption.
- **Real-hardware testing is per-OS and per-owner.** The write path especially
  must be exercised on a real removable stick on each OS (that's how the Linux
  `O_EXCL` bug was caught). Never test-write to a system disk.

## Current status snapshot

- **Shared core:** domain types, traits, `Reporter`, hashing, raw write+verify
  engine — done and unit-tested (runs on both OSes).
- **Linux backend:** device enumeration + raw write — done, verified on real
  hardware.
- **Windows backend:** **experimental** — enumeration (`\\.\PhysicalDriveN` +
  `IOCTL_STORAGE_QUERY_PROPERTY`) and disk I/O (`CreateFileW` + `DeviceIoControl`
  + volume lock/dismount) are implemented in `platform/src/windows` against the
  `windows` crate. Type-checks for `x86_64-pc-windows-gnu` but has **not been run
  on real Windows** — that's the Windows side's job: build (`cargo build`),
  `list`, then `write` to a spare stick, and refine (sector alignment, error
  messages). ISO `create` needs a cross-platform ISO reader (cdfs is Unix-only).
- **CLI/GUI:** shared, build on both OSes; on Windows `create`/ISO-`inspect`
  report "not supported yet" until a cross-platform ISO reader lands.

> Cross-checking the Windows build from Linux: `rustup target add
> x86_64-pc-windows-gnu`, install mingw-w64, then
> `cargo check --target x86_64-pc-windows-gnu -p usbforge-core -p usbforge-platform -p usbforge-cli`
> (type-checks without a Windows box; CI's `windows-latest` job does the real build).
