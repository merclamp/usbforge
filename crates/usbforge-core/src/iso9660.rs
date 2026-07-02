//! A small, pure-Rust ISO9660 reader (with Joliet) — no external crate, so it
//! builds on every platform. Enough for USBForge: walk the directory tree, read
//! files, look a path up, and read the volume label. Reads from a seekable
//! source and never loads the whole image into memory.

use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::{Error, Result};

const SECTOR: u64 = 2048;

/// One directory entry (file or directory).
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    /// Starting logical block (sector) of the entry's data.
    pub extent: u32,
    /// Data length in bytes.
    pub size: u32,
}

/// An opened ISO9660 image.
pub struct Iso {
    file: RefCell<File>,
    root: DirEntry,
    joliet: bool,
    volume_label: String,
}

impl Iso {
    /// Parse the volume descriptors and locate the root directory (preferring
    /// the Joliet supplementary descriptor for proper long/case names).
    pub fn open(mut file: File) -> Result<Iso> {
        let mut pvd_root: Option<DirEntry> = None;
        let mut joliet_root: Option<DirEntry> = None;
        let mut label = String::new();

        // Volume Descriptor Set starts at sector 16.
        for lba in 16u64..48 {
            let mut sec = [0u8; SECTOR as usize];
            file.seek(SeekFrom::Start(lba * SECTOR))?;
            if file.read_exact(&mut sec).is_err() {
                break;
            }
            if &sec[1..6] != b"CD001" {
                break;
            }
            match sec[0] {
                1 => {
                    // Primary Volume Descriptor
                    pvd_root = Some(parse_dir_record(&sec[156..156 + 34])?);
                    label = decode_ascii(&sec[40..72]);
                }
                2 => {
                    // Supplementary Volume Descriptor — Joliet if the escape
                    // sequence selects UCS-2.
                    if is_joliet_escape(&sec[88..120]) {
                        joliet_root = Some(parse_dir_record(&sec[156..156 + 34])?);
                    }
                }
                255 => break, // Volume Descriptor Set Terminator
                _ => {}
            }
        }

        let joliet = joliet_root.is_some();
        let root = joliet_root
            .or(pvd_root)
            .ok_or_else(|| Error::Image("no ISO9660 primary volume descriptor".into()))?;

        Ok(Iso {
            file: RefCell::new(file),
            root,
            joliet,
            volume_label: label.trim().to_string(),
        })
    }

    pub fn root(&self) -> DirEntry {
        self.root.clone()
    }

    pub fn volume_label(&self) -> &str {
        &self.volume_label
    }

    /// List the entries of a directory (excluding the `.`/`..` records).
    pub fn read_dir(&self, dir: &DirEntry) -> Result<Vec<DirEntry>> {
        let mut data = vec![0u8; dir.size as usize];
        {
            let mut f = self.file.borrow_mut();
            f.seek(SeekFrom::Start(dir.extent as u64 * SECTOR))?;
            f.read_exact(&mut data)?;
        }

        let mut entries = Vec::new();
        let mut pos = 0usize;
        while pos < data.len() {
            let len = data[pos] as usize;
            if len == 0 {
                // Directory records never span a sector; a zero length means
                // padding to the next sector.
                let next = (pos / SECTOR as usize + 1) * SECTOR as usize;
                if next <= pos {
                    break;
                }
                pos = next;
                continue;
            }
            if len < 34 || pos + len > data.len() {
                break;
            }
            let rec = &data[pos..pos + len];
            let id_len = rec[32] as usize;
            if 33 + id_len > len {
                break;
            }
            let id = &rec[33..33 + id_len];

            // Skip the "." (0x00) and ".." (0x01) self/parent records.
            if id_len == 1 && (id[0] == 0 || id[0] == 1) {
                pos += len;
                continue;
            }

            let is_dir = rec[25] & 0x02 != 0;
            let extent = u32::from_le_bytes([rec[2], rec[3], rec[4], rec[5]]);
            let size = u32::from_le_bytes([rec[10], rec[11], rec[12], rec[13]]);
            let name = if self.joliet {
                decode_joliet(id)
            } else {
                clean_version(&String::from_utf8_lossy(id))
            };

            entries.push(DirEntry {
                name,
                is_dir,
                extent,
                size,
            });
            pos += len;
        }
        Ok(entries)
    }

    /// Stream a file's contents into `out`, invoking `on_progress` after each
    /// chunk with the running byte count. Lets callers report smooth progress
    /// while a single large file (e.g. a multi-GiB squashfs) streams out.
    /// Returns bytes written.
    pub fn copy_file_with<W: Write>(
        &self,
        entry: &DirEntry,
        out: &mut W,
        mut on_progress: impl FnMut(u64),
    ) -> Result<u64> {
        let mut f = self.file.borrow_mut();
        f.seek(SeekFrom::Start(entry.extent as u64 * SECTOR))?;
        let mut remaining = u64::from(entry.size);
        let mut buf = vec![0u8; 256 * 1024];
        let mut total = 0u64;
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            f.read_exact(&mut buf[..want])?;
            out.write_all(&buf[..want])?;
            remaining -= want as u64;
            total += want as u64;
            on_progress(total);
        }
        Ok(total)
    }

    /// Look up an absolute `/`-separated path (case-insensitive).
    pub fn find(&self, path: impl AsRef<Path>) -> Result<Option<DirEntry>> {
        let path = path.as_ref().to_string_lossy().replace('\\', "/");
        let mut current = self.root.clone();
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            if !current.is_dir {
                return Ok(None);
            }
            match self
                .read_dir(&current)?
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(comp))
            {
                Some(e) => current = e,
                None => return Ok(None),
            }
        }
        Ok(Some(current))
    }
}

fn parse_dir_record(rec: &[u8]) -> Result<DirEntry> {
    if rec.len() < 34 {
        return Err(Error::Image("truncated directory record".into()));
    }
    Ok(DirEntry {
        name: String::new(),
        is_dir: rec[25] & 0x02 != 0,
        extent: u32::from_le_bytes([rec[2], rec[3], rec[4], rec[5]]),
        size: u32::from_le_bytes([rec[10], rec[11], rec[12], rec[13]]),
    })
}

/// Joliet escape sequences that select UCS-2: `%/@`, `%/C`, `%/E`.
fn is_joliet_escape(esc: &[u8]) -> bool {
    esc.windows(3)
        .any(|w| w == b"%/@" || w == b"%/C" || w == b"%/E")
}

fn decode_joliet(id: &[u8]) -> String {
    let units: Vec<u16> = id
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    clean_version(&String::from_utf16_lossy(&units))
}

fn decode_ascii(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// Strip the ISO9660 `;version` suffix and any trailing dot.
fn clean_version(s: &str) -> String {
    let base = s.split(';').next().unwrap_or(s);
    base.trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::clean_version;

    #[test]
    fn version_cleaning() {
        assert_eq!(clean_version("README.TXT;1"), "README.TXT");
        assert_eq!(clean_version("DIR."), "DIR");
        assert_eq!(clean_version("file"), "file");
    }
}
