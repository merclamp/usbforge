//! USBForge graphical frontend (Slint).
//!
//! A thin view over the same `usbforge-core` engine the CLI drives. Device
//! enumeration and image selection happen on the UI thread; the actual
//! write/format/create runs on a worker thread and reports progress + log lines
//! back to the UI via [`GuiReporter`] (marshalled onto the event loop).
//!
//! Hide the console window on Windows for release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use slint::{ModelRc, SharedString, VecModel};

use usbforge_core::device::{humanize_bytes, Device};
use usbforge_core::disk::Access;
use usbforge_core::filesystem::{FileSystem, PartitionScheme};
use usbforge_core::format::{self, PartitionSlice};
use usbforge_core::iso::IsoReader;
use usbforge_core::layout;
use usbforge_core::report::{Level, Reporter};
use usbforge_core::write::{self, WriteOptions};

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = AppWindow::new()?;

    // The enumerated devices, mapped 1:1 to the dropdown entries.
    let devices: Rc<RefCell<Vec<Device>>> = Rc::new(RefCell::new(Vec::new()));
    refresh_devices(&app, &devices);

    // Refresh button.
    {
        let weak = app.as_weak();
        let devices = devices.clone();
        app.on_refresh(move || {
            if let Some(app) = weak.upgrade() {
                refresh_devices(&app, &devices);
            }
        });
    }

    // Browse button — native file dialog via rfd.
    {
        let weak = app.as_weak();
        app.on_browse(move || {
            if let Some(file) = rfd::FileDialog::new()
                .add_filter(
                    "Disk images",
                    &[
                        "iso", "img", "raw", "dd", "bin", "vhd", "vhdx", "gz", "xz", "zst",
                    ],
                )
                .add_filter("All files", &["*"])
                .pick_file()
            {
                if let Some(app) = weak.upgrade() {
                    app.set_image_path(file.display().to_string().into());
                }
            }
        });
    }

    // START — validate, then run the operation on a worker thread.
    {
        let weak = app.as_weak();
        let devices = devices.clone();
        app.on_start(move || {
            let app = match weak.upgrade() {
                Some(a) => a,
                None => return,
            };

            let idx = app.get_device_index();
            let device = match (idx >= 0)
                .then(|| devices.borrow().get(idx as usize).cloned())
                .flatten()
            {
                Some(dev) => dev,
                None => {
                    app.set_status("Select a device first.".into());
                    return;
                }
            };

            let mode = app.get_mode_index();
            let scheme = if app.get_scheme_index() == 0 {
                PartitionScheme::Gpt
            } else {
                PartitionScheme::Mbr
            };
            let image = app.get_image_path().to_string();
            let label = {
                let l = app.get_volume_label().to_string();
                if l.trim().is_empty() {
                    "USBFORGE".to_string()
                } else {
                    l
                }
            };

            if device.read_only {
                app.set_status(format!("{} is write-protected.", device.path).into());
                return;
            }
            if (mode == 0 || mode == 1) && image.trim().is_empty() {
                app.set_status("Choose an image file first.".into());
                return;
            }

            app.set_busy(true);
            app.set_progress(0.0);
            app.set_progress_text("0%".into());
            app.set_log(SharedString::from(""));
            app.set_status(format!("Working on {} …", device.path).into());

            let reporter = Arc::new(GuiReporter {
                weak: weak.clone(),
                last_pct: AtomicI32::new(-1),
            });
            let done = weak.clone();
            std::thread::spawn(move || {
                let result =
                    run_operation(mode, &device, scheme, &image, &label, reporter.as_ref());
                let (ok, msg) = match result {
                    Ok(s) => (true, format!("✓ {s}")),
                    Err(e) => (false, format!("✗ {e}")),
                };
                let _ = done.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    app.set_status(msg.into());
                    if ok {
                        app.set_progress(1.0);
                        app.set_progress_text("100%".into());
                    }
                });
            });
        });
    }

    app.run()?;
    Ok(())
}

/// Re-enumerate removable devices and update the dropdown + backing list.
fn refresh_devices(app: &AppWindow, store: &Rc<RefCell<Vec<Device>>>) {
    let list = usbforge_platform::device_enumerator()
        .list(true)
        .unwrap_or_default();
    let labels: Vec<SharedString> = list
        .iter()
        .map(|d| {
            SharedString::from(format!(
                "{}  [{}]  {}  {}",
                d.path,
                d.bus,
                d.size_human(),
                d.display_name()
            ))
        })
        .collect();
    if labels.is_empty() {
        app.set_status("No removable devices found. Plug in a USB stick and press Refresh.".into());
    } else {
        app.set_status("Ready.".into());
    }
    app.set_devices(ModelRc::new(VecModel::from(labels)));
    *store.borrow_mut() = list;
}

/// Run the selected operation. Called on a worker thread.
fn run_operation(
    mode: i32,
    device: &Device,
    scheme: PartitionScheme,
    image: &str,
    label: &str,
    reporter: &dyn Reporter,
) -> usbforge_core::Result<String> {
    let access = usbforge_platform::disk_access();
    match mode {
        0 => {
            // Create bootable USB from ISO (file-copy → FAT32 ESP).
            let reader = IsoReader::open(image)?;
            let mut target = access.open(device, Access::ReadWriteExclusive)?;
            let stats = reader.install_to_device(&mut *target, scheme, label, reporter)?;
            target.sync()?;
            Ok(format!(
                "Created bootable USB — {} files ({}).",
                stats.files,
                humanize_bytes(stats.bytes)
            ))
        }
        1 => {
            // Raw (dd-style) write with verification.
            let mut target = access.open(device, Access::ReadWriteExclusive)?;
            let summary = write::write_image_file(
                std::path::Path::new(image),
                &mut *target,
                &WriteOptions::default(),
                reporter,
            )?;
            target.sync()?;
            Ok(format!(
                "Wrote {}{}.",
                humanize_bytes(summary.bytes_written),
                if summary.verified { " (verified)" } else { "" }
            ))
        }
        _ => {
            // Format only (GPT/MBR + FAT32).
            let mut target = access.open(device, Access::ReadWriteExclusive)?;
            let region = layout::write_single_partition(
                &mut *target,
                scheme,
                FileSystem::Fat32,
                label,
                false,
            )?;
            {
                let mut slice = PartitionSlice::new(&mut *target, region.start, region.len);
                format::format_fat32(&mut slice, label)?;
            }
            target.sync()?;
            Ok("Formatted as FAT32.".into())
        }
    }
}

/// Reporter that marshals log lines and (throttled) progress to the UI thread.
struct GuiReporter {
    weak: slint::Weak<AppWindow>,
    last_pct: AtomicI32,
}

impl Reporter for GuiReporter {
    fn log(&self, level: Level, message: &str) {
        let line = format!("[{level}] {message}\n");
        let weak = self.weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                let mut s = app.get_log().to_string();
                s.push_str(&line);
                app.set_log(s.into());
            }
        });
    }

    fn progress(&self, _operation: &str, fraction: f32) {
        // Only push when the whole-percent value changes (avoid flooding the loop).
        let pct = (fraction * 100.0) as i32;
        if self.last_pct.swap(pct, Ordering::Relaxed) == pct {
            return;
        }
        let frac = fraction.clamp(0.0, 1.0);
        let text = format!("{pct}%");
        let weak = self.weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_progress(frac);
                app.set_progress_text(text.into());
            }
        });
    }
}
