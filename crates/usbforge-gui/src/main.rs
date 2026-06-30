//! USBForge graphical frontend — placeholder.
//!
//! The real UI (see `ARCHITECTURE.md` for the toolkit decision) will bind the
//! same `usbforge-core` traits the CLI uses: a device dropdown fed by
//! `device_enumerator()`, a source picker backed by `image::ImageInfo`, and a
//! progress bar driven by a `Reporter` implementation that marshals updates to
//! the UI thread. For now this binary just proves the crate wiring and shows
//! what the GUI will drive.

fn main() {
    println!("{} GUI — not implemented yet.", usbforge_core::PRODUCT);
    println!("Use the CLI for now:  usbforge list");

    match usbforge_platform::device_enumerator().list(true) {
        Ok(devices) => println!("Detected {} removable device(s).", devices.len()),
        Err(e) => eprintln!("Device enumeration failed: {e}"),
    }
}
