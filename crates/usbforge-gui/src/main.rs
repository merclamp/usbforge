//! USBForge graphical frontend (Slint).
//!
//! Device enumeration runs in-process (no privileges needed). The destructive
//! operations are delegated to the `usbforge` CLI, launched via **pkexec** so
//! PolicyKit shows a native auth dialog and the work runs as root — the GUI
//! itself never needs to be root. The CLI's stdout + stderr are parsed as it
//! runs to drive the progress bar; the window shows a phase status + that bar.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use slint::{ModelRc, SharedString, VecModel};

use usbforge_core::device::Device;

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = AppWindow::new()?;

    let devices: Rc<RefCell<Vec<Device>>> = Rc::new(RefCell::new(Vec::new()));
    refresh_devices(&app, &devices);

    {
        let weak = app.as_weak();
        let devices = devices.clone();
        app.on_refresh(move || {
            if let Some(app) = weak.upgrade() {
                refresh_devices(&app, &devices);
            }
        });
    }

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
                Some(d) => d,
                None => {
                    app.set_status("Select a device first.".into());
                    return;
                }
            };

            let mode = app.get_mode_index();
            let image = app.get_image_path().to_string();
            if (mode == 0 || mode == 1) && image.trim().is_empty() {
                app.set_status("Choose an image file first.".into());
                return;
            }

            let args = build_cli_args(
                mode,
                app.get_scheme_index(),
                app.get_create_fs_index(),
                &image,
                &device.path,
                {
                    let l = app.get_volume_label().to_string();
                    if l.trim().is_empty() {
                        "USBFORGE".to_string()
                    } else {
                        l
                    }
                },
            );

            app.set_busy(true);
            app.set_progress(0.0);
            app.set_progress_text("0%".into());
            app.set_status(format!("Authorising access to {} …", device.path).into());

            let working = working_status(mode, &device.path);
            let weak = weak.clone();
            std::thread::spawn(move || run_cli(args, weak, true, working));
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

/// The "working" status shown once the operation is actually running, phrased
/// for the selected mode (kept in sync with the modes in [`build_cli_args`]:
/// 0 = create, 1 = write, 2 = format).
fn working_status(mode: i32, device_path: &str) -> String {
    match mode {
        0 => format!("Creating bootable drive on {device_path} …"),
        1 => format!("Writing image to {device_path} …"),
        _ => format!("Formatting {device_path} …"),
    }
}

/// Build the `usbforge` CLI argument vector for the selected operation.
fn build_cli_args(
    mode: i32,
    scheme_index: i32,
    create_fs_index: i32,
    image: &str,
    device_path: &str,
    label: String,
) -> Vec<String> {
    let scheme = if scheme_index == 0 { "gpt" } else { "mbr" };
    match mode {
        0 => {
            let fs = match create_fs_index {
                1 => "fat32",
                2 => "ntfs",
                _ => "auto",
            };
            vec![
                "create".into(),
                image.into(),
                device_path.into(),
                "--scheme".into(),
                scheme.into(),
                "--fs".into(),
                fs.into(),
                "--label".into(),
                label,
                "--yes".into(),
            ]
        }
        1 => vec![
            "write".into(),
            image.into(),
            device_path.into(),
            "--yes".into(),
        ],
        _ => vec![
            "format".into(),
            device_path.into(),
            "--scheme".into(),
            scheme.into(),
            "--fs".into(),
            "fat32".into(),
            "--label".into(),
            label,
            "--yes".into(),
        ],
    }
}

/// Launch the CLI and stream its output to the UI. When `elevate` is set the
/// command runs via pkexec (needed for device operations).
fn run_cli(args: Vec<String>, weak: slint::Weak<AppWindow>, elevate: bool, working_status: String) {
    let cli = cli_path();
    let (program, full_args) = if elevate && tool_exists("pkexec") {
        let mut a = vec![cli.to_string_lossy().to_string()];
        a.extend(args);
        ("pkexec".to_string(), a)
    } else {
        (cli.to_string_lossy().to_string(), args)
    };

    let mut child = match Command::new(&program)
        .args(&full_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            finish(&weak, false, format!("Failed to launch `{program}`: {e}"));
            return;
        }
    };

    // Stream stdout and stderr concurrently, line by line, so everything the CLI
    // prints shows up live and in order — not dumped as one blob at the end.
    // (Draining stderr fully *before* touching stdout also risked a pipe-buffer
    // deadlock, and could drop the CLI's unterminated final `\r…100%` line.)
    let stderr = child.stderr.take();
    let stdout = child.stdout.take();

    // Flip the status from "Authorising …" to the working label the moment the
    // first output arrives (i.e. pkexec auth passed and the CLI is running), so
    // the window stops saying "Authorising" while it's actually working.
    let started = Arc::new(AtomicBool::new(false));
    let working = Arc::new(working_status);

    let err_join = {
        let weak = weak.clone();
        let started = started.clone();
        let working = working.clone();
        std::thread::spawn(move || {
            if let Some(e) = stderr {
                pump(e, &weak, &started, &working);
            }
        })
    };

    // stdout on this thread; keep its last non-empty line for the status message.
    let mut last_stdout = String::new();
    if let Some(o) = stdout {
        last_stdout = pump(o, &weak, &started, &working);
    }
    let _ = err_join.join();

    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    let msg = if ok {
        if last_stdout.is_empty() {
            "Done.".to_string()
        } else {
            last_stdout
        }
    } else {
        "Operation failed or was cancelled.".to_string()
    };

    finish(&weak, ok, msg);
}

/// Read a child stream byte-by-byte, splitting on `\n`/`\r`, and feed each line
/// to the progress parser. Flushes a trailing unterminated line at EOF — the
/// CLI's final `\r…100%` progress carries no newline — so the last line is never
/// dropped. Returns the last non-empty line seen (the caller uses stdout's for
/// the finish message).
fn pump(
    stream: impl Read,
    weak: &slint::Weak<AppWindow>,
    started: &AtomicBool,
    working_status: &str,
) -> String {
    let mut reader = std::io::BufReader::new(stream);
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    let mut last = String::new();
    let flush = |buf: &mut Vec<u8>, last: &mut String| {
        if buf.is_empty() {
            return;
        }
        let line = String::from_utf8_lossy(buf).trim().to_string();
        buf.clear();
        if !line.is_empty() {
            // First real output means auth passed and work has begun.
            if !started.swap(true, Ordering::Relaxed) {
                set_status(weak, working_status);
            }
            *last = line.clone();
            handle_line(&line, weak);
        }
    };
    loop {
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if byte[0] == b'\n' || byte[0] == b'\r' {
                    flush(&mut buf, &mut last);
                } else {
                    buf.push(byte[0]);
                }
            }
            Err(_) => break,
        }
    }
    flush(&mut buf, &mut last); // trailing line with no terminator
    last
}

/// A `op: NN%` line advances the progress bar; any other line is ignored (the
/// window shows the phase status + bar, not a full log).
fn handle_line(line: &str, weak: &slint::Weak<AppWindow>) {
    if let Some(prefix) = line.strip_suffix('%') {
        if let Some(num) = prefix.rsplit([' ', ':']).next() {
            if let Ok(pct) = num.trim().parse::<f32>() {
                let frac = (pct / 100.0).clamp(0.0, 1.0);
                let text = format!("{}%", pct as i32);
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_progress(frac);
                        app.set_progress_text(text.into());
                    }
                });
            }
        }
    }
}

fn set_status(weak: &slint::Weak<AppWindow>, status: &str) {
    let status = status.to_string();
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            app.set_status(status.into());
        }
    });
}

fn finish(weak: &slint::Weak<AppWindow>, ok: bool, msg: String) {
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            app.set_busy(false);
            app.set_status(
                if ok {
                    format!("✓ {msg}")
                } else {
                    format!("✗ {msg}")
                }
                .into(),
            );
            if ok {
                app.set_progress(1.0);
                app.set_progress_text("100%".into());
            }
        }
    });
}

/// Path to the sibling `usbforge` CLI binary (falls back to `PATH`).
fn cli_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("usbforge");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from("usbforge")
}

fn tool_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(name).is_file()))
        .unwrap_or(false)
}
