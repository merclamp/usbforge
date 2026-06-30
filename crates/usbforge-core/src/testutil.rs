//! Test-only helpers shared across the core unit tests.

use std::io::{Cursor, Read, Seek, SeekFrom, Write};

use crate::disk::BlockDevice;
use crate::Result;

/// An in-memory, fixed-size block device. Lets us exercise the write/partition/
/// format engines without touching real hardware.
pub struct MemDevice {
    cur: Cursor<Vec<u8>>,
    sector: u32,
}

impl MemDevice {
    pub fn new(size: usize) -> Self {
        MemDevice {
            cur: Cursor::new(vec![0u8; size]),
            sector: 512,
        }
    }

    /// Borrow the backing bytes (for assertions). Named `data` rather than
    /// `bytes` to avoid colliding with `std::io::Read::bytes`.
    pub fn data(&self) -> &[u8] {
        self.cur.get_ref()
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
