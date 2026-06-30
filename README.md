# USBForge

A cross-platform (Linux + Windows) utility for creating bootable USB drives and
writing disk images — a clean-room **Rust** reimagining of
[Rufus](https://github.com/pbatard/rufus), carrying over its feature set onto a
portable, memory-safe codebase.

> Status: **early, but it writes.** The workspace builds; the CLI enumerates
> removable devices, verifies checksums, and writes raw images (`.iso`/`.img`/
> `.raw`) to a device dd-style with progress, flush, and read-back verification —
> guarded so it refuses fixed/system disks. Formatting, bootloaders, the Windows
> backend, and the GUI are next — see [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Why a rewrite (not a port)

Rufus is ~46k lines of C built directly on the Win32 API at every layer (GUI,
device enumeration via SetupAPI, disk I/O via `DeviceIoControl`, VDS COM). A
straight port means rewriting the platform and GUI layers anyway. Rust lets us
do that once, cleanly: traits replace the C "HAL", `cfg(target_os)` replaces
`#ifdef _WIN32`, and the GUI is decoupled from the engine via explicit
reporting seams instead of global `uprintf()` / `SendMessage` glue.

## Workspace layout

| Crate | Role |
|-------|------|
| `usbforge-core` | Platform-agnostic domain model, traits (HAL), hashing, reporting. No OS or GUI code. |
| `usbforge-platform` | Per-OS backends behind the core traits (Linux: sysfs/ioctl; Windows: SetupAPI/DeviceIoControl). |
| `usbforge-cli` | Headless frontend + proof-of-concept (`usbforge` binary). |
| `usbforge-gui` | Slint GUI: device picker, write/format/create, live progress + log. |

## Contributing (two-person, cross-platform)

The project is developed in parallel on Linux and Windows. Shared, OS-neutral
code lives in `usbforge-core` and the frontends; OS-specific code lives only in
`usbforge-platform` behind `cfg(target_os)`. The trait set in `usbforge-core` is
the contract both backends implement. See **[`docs/WORK-SPLIT.md`](docs/WORK-SPLIT.md)**
for the full ownership map, per-milestone split, and a Windows quick-start.
CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) builds + tests on
both Linux and Windows on every push.

## Build & run

```sh
cargo build
cargo run -p usbforge-cli -- list           # removable devices only
cargo run -p usbforge-cli -- list --all     # include fixed disks
cargo run -p usbforge-cli -- hash path/to/image.iso
cargo run -p usbforge-cli -- inspect path/to/disk.iso   # ISOs: label, files, UEFI/BIOS bootability

# The destructive operations need root, prompt for confirmation unless --yes,
# and refuse fixed/system disks unless --allow-fixed.

# Raw (dd-style) write of an image to a device, with read-back verification:
sudo target/release/usbforge write path/to/image.iso /dev/sdX

# Partition + format a device (GPT/MBR, FAT32):
sudo target/release/usbforge format /dev/sdX --scheme gpt --fs fat32 --label MYUSB

# Create a UEFI-bootable USB from an ISO (file-copy onto a FAT32 ESP):
sudo target/release/usbforge create path/to/distro.iso /dev/sdX

# Windows ISO (install.wim > 4 GiB)? Use NTFS + UEFI:NTFS (needs ntfs-3g):
sudo target/release/usbforge create win.iso /dev/sdX --fs ntfs   # or --fs auto

# For BIOS *and* UEFI boot of an isohybrid ISO (most Linux distros), raw-write it
# — `inspect` reports "isohybrid: yes" when this applies:
sudo target/release/usbforge write path/to/distro.iso /dev/sdX

cargo test
```

### GUI

A Slint desktop GUI wraps the same engine (device dropdown, image picker,
write/format/create modes, progress bar + log):

```sh
cargo run -p usbforge-gui          # enumerate works as a user;
pkexec target/debug/usbforge-gui   # …run elevated to write to devices
```

Linux build needs `libgtk-3-dev libxkbcommon-dev libfontconfig1-dev` (and a
Wayland/X11 session at runtime).

Listing reads `/sys/block`; no root needed. `write` opens the device with
`O_EXCL` (fails if a partition is mounted) and needs elevated privileges.

## License

GPL-3.0-or-later, matching upstream Rufus (whose GPLv3 algorithms — ms-sys boot
records, FAT layout — we may reuse with attribution).
