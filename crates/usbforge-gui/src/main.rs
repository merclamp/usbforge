//! USBForge graphical frontend (Slint).
//!
//! Device enumeration runs in-process (no privileges needed). The destructive
//! operations are delegated to the `usbforge` CLI, launched via **pkexec** so
//! PolicyKit shows a native auth dialog and the work runs as root — the GUI
//! itself never needs to be root. The CLI's progress/log (on stderr) is streamed
//! back into the window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;

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
            app.set_log(SharedString::from(""));
            app.set_status(format!("Authorising and working on {} …", device.path).into());

            let weak = weak.clone();
            std::thread::spawn(move || run_privileged(args, weak));
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

/// Launch the CLI (via pkexec when available) and stream its output to the UI.
fn run_privileged(args: Vec<String>, weak: slint::Weak<AppWindow>) {
    let cli = cli_path();
    let (program, full_args) = if tool_exists("pkexec") {
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

    if let Some(stderr) = child.stderr.take() {
        stream_output(stderr, &weak);
    }

    let mut stdout = String::new();
    if let Some(mut so) = child.stdout.take() {
        let _ = so.read_to_string(&mut stdout);
    }
    if !stdout.trim().is_empty() {
        append_log(&weak, stdout.trim());
    }

    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    let msg = if ok {
        stdout
            .lines()
            .last()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "Done.".to_string())
    } else {
        "Operation failed or was cancelled — see the log.".to_string()
    };
    finish(&weak, ok, msg);
}

/// Read the child's stderr, splitting on `\n`/`\r`, and route lines to either the
/// progress bar (`op: NN%`) or the log.
fn stream_output(stderr: std::process::ChildStderr, weak: &slint::Weak<AppWindow>) {
    let mut reader = std::io::BufReader::new(stderr);
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if byte[0] == b'\n' || byte[0] == b'\r' {
                    if !buf.is_empty() {
                        let line = String::from_utf8_lossy(&buf).trim().to_string();
                        if !line.is_empty() {
                            handle_line(&line, weak);
                        }
                        buf.clear();
                    }
                } else {
                    buf.push(byte[0]);
                }
            }
            Err(_) => break,
        }
    }
}

/// A line of CLI output → progress update or log line.
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
                return;
            }
        }
    }
    append_log(weak, line);
}

fn append_log(weak: &slint::Weak<AppWindow>, line: &str) {
    let line = format!("{line}\n");
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            let mut s = app.get_log().to_string();
            s.push_str(&line);
            app.set_log(s.into());
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
