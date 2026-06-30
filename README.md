# USBForge

A cross-platform (Linux + Windows) utility for creating bootable USB drives and
writing disk images — a clean-room **Rust** reimagining of
[Rufus](https://github.com/pbatard/rufus), carrying over its feature set onto a
portable, memory-safe codebase.

> Status: **early scaffold.** The workspace builds and the CLI can enumerate
> removable devices and verify image checksums. The write path, formatting,
> bootloaders, and GUI are under construction — see
> [`ARCHITECTURE.md`](ARCHITECTURE.md) for the design and roadmap.

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
| `usbforge-gui` | Graphical frontend (placeholder for now). |

## Build & run

```sh
cargo build
cargo run -p usbforge-cli -- list           # removable devices only
cargo run -p usbforge-cli -- list --all     # include fixed disks
cargo run -p usbforge-cli -- hash path/to/image.iso
cargo run -p usbforge-cli -- inspect path/to/image.iso
cargo test
```

Listing reads `/sys/block`; no root needed. Writing to a device (later) will.

## License

GPL-3.0-or-later, matching upstream Rufus (whose GPLv3 algorithms — ms-sys boot
records, FAT layout — we may reuse with attribution).
