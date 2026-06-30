//! Raw image writer (the "dd-style" write path).
//!
//! Copies a source image byte-for-byte onto a [`BlockDevice`], flushes it down
//! to the medium, and optionally verifies by reading the device back and
//! comparing against the source. This is the portable counterpart to Rufus's
//! raw write in `format.c` / `WriteDrive`, minus the Win32 I/O.
//!
//! Safety policy (size check, write-protect) lives partly here and partly in the
//! frontend: the engine refuses an image that does not fit, but the "is this a
//! system disk?" decision is the frontend's (it has the [`crate::device::Device`]
//! metadata).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::device::humanize_bytes;
use crate::disk::BlockDevice;
use crate::report::{Reporter, ReporterExt};
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct WriteOptions {
    /// Read the device back after writing and compare with the source.
    pub verify: bool,
    /// Copy buffer size in bytes.
    pub buffer_size: usize,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            verify: true,
            buffer_size: 4 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WriteSummary {
    pub bytes_written: u64,
    pub verified: bool,
}

/// Write a raw image file (`.iso`/`.img`/`.raw`) onto `target`.
///
/// Reports progress under the operation names `"write"` and (if enabled)
/// `"verify"`. Returns once data is flushed (and verified). The caller is
/// responsible for having vetted that `target` is the right, removable device.
pub fn write_image_file(
    image_path: &Path,
    target: &mut dyn BlockDevice,
    opts: &WriteOptions,
    reporter: &dyn Reporter,
) -> Result<WriteSummary> {
    let mut source = File::open(image_path)?;
    let image_len = source.metadata()?.len();
    let device_size = target.size();

    if device_size != 0 && image_len > device_size {
        return Err(Error::Refused(format!(
            "image ({}) is larger than the device ({})",
            humanize_bytes(image_len),
            humanize_bytes(device_size)
        )));
    }

    reporter.info(&format!(
        "Writing {} to device …",
        humanize_bytes(image_len)
    ));
    let bytes_written = copy_with_progress(
        &mut source,
        target,
        image_len,
        opts.buffer_size,
        "write",
        reporter,
    )?;

    reporter.info("Flushing buffers to the medium …");
    target.flush().ok();
    target.sync()?;

    let verified = if opts.verify {
        reporter.info("Verifying written data …");
        source.seek(SeekFrom::Start(0))?;
        target.seek(SeekFrom::Start(0))?;
        verify_against(&mut source, target, image_len, opts.buffer_size, reporter)?;
        true
    } else {
        false
    };

    Ok(WriteSummary {
        bytes_written,
        verified,
    })
}

/// Copy exactly `total` bytes (source EOF) from `src` to `dst`, reporting
/// fractional progress under `op`.
fn copy_with_progress(
    src: &mut dyn Read,
    dst: &mut dyn Write,
    total: u64,
    buffer_size: usize,
    op: &str,
    reporter: &dyn Reporter,
) -> Result<u64> {
    let mut buf = vec![0u8; buffer_size];
    let mut done: u64 = 0;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])?;
        done += n as u64;
        if total > 0 {
            reporter.progress(op, done as f32 / total as f32);
        }
    }
    reporter.progress(op, 1.0);
    Ok(done)
}

/// Read `total` bytes from both streams and compare. Returns an error on the
/// first mismatch.
fn verify_against(
    src: &mut dyn Read,
    dst: &mut dyn Read,
    total: u64,
    buffer_size: usize,
    reporter: &dyn Reporter,
) -> Result<()> {
    let mut a = vec![0u8; buffer_size];
    let mut b = vec![0u8; buffer_size];
    let mut done: u64 = 0;
    while done < total {
        let want = buffer_size.min((total - done) as usize);
        src.read_exact(&mut a[..want])?;
        dst.read_exact(&mut b[..want])?;
        if a[..want] != b[..want] {
            // Pinpoint the first differing byte for a useful message.
            let off = a[..want]
                .iter()
                .zip(&b[..want])
                .position(|(x, y)| x != y)
                .unwrap_or(0) as u64;
            return Err(Error::Other(format!(
                "verification failed at offset {}",
                done + off
            )));
        }
        done += want as u64;
        if total > 0 {
            reporter.progress("verify", done as f32 / total as f32);
        }
    }
    reporter.progress("verify", 1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::BlockDevice;
    use crate::report::NullReporter;
    use std::io::Cursor;

    /// An in-memory fixed-size block device for tests (never touches hardware).
    struct MemDevice {
        cur: Cursor<Vec<u8>>,
        sector: u32,
    }
    impl MemDevice {
        fn new(size: usize) -> Self {
            MemDevice {
                cur: Cursor::new(vec![0u8; size]),
                sector: 512,
            }
        }
    }
    impl Read for MemDevice {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.cur.read(buf)
        }
    }
    impl Write for MemDevice {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.cur.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Seek for MemDevice {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.cur.seek(pos)
        }
    }
    impl BlockDevice for MemDevice {
        fn sector_size(&self) -> u32 {
            self.sector
        }
        fn size(&self) -> u64 {
            self.cur.get_ref().len() as u64
        }
        fn sync(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn temp_image(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(name);
        File::create(&p).unwrap().write_all(bytes).unwrap();
        p
    }

    #[test]
    fn write_and_verify_roundtrip() {
        // Pseudo-random-ish but deterministic payload.
        let payload: Vec<u8> = (0..(1024 * 1024)).map(|i| (i * 31 + 7) as u8).collect();
        let img = temp_image("usbforge_write_ok.img", &payload);
        let mut dev = MemDevice::new(4 * 1024 * 1024);

        let summary = write_image_file(
            &img,
            &mut dev,
            &WriteOptions {
                verify: true,
                buffer_size: 64 * 1024,
            },
            &NullReporter,
        )
        .unwrap();

        assert_eq!(summary.bytes_written, payload.len() as u64);
        assert!(summary.verified);
        assert_eq!(&dev.cur.get_ref()[..payload.len()], &payload[..]);
        let _ = std::fs::remove_file(&img);
    }

    #[test]
    fn refuses_image_larger_than_device() {
        let payload = vec![0xABu8; 2 * 1024 * 1024];
        let img = temp_image("usbforge_write_toobig.img", &payload);
        let mut dev = MemDevice::new(1024 * 1024); // smaller than image

        let err =
            write_image_file(&img, &mut dev, &WriteOptions::default(), &NullReporter).unwrap_err();
        assert!(matches!(err, Error::Refused(_)));
        let _ = std::fs::remove_file(&img);
    }
}
