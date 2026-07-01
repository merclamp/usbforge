//! Raw physical-drive access on Windows.

use std::ffi::c_void;
use std::io::{self, Read, Seek, SeekFrom, Write};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, ReadFile, SetFilePointerEx, WriteFile, FILE_BEGIN,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{
    FSCTL_ALLOW_EXTENDED_DASD_IO, FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME,
    IOCTL_STORAGE_GET_DEVICE_NUMBER, STORAGE_DEVICE_NUMBER,
};
use windows::Win32::System::IO::DeviceIoControl;

use usbforge_core::device::Device;
use usbforge_core::disk::{Access, BlockDevice, DiskAccess};
use usbforge_core::{Error, Result};

pub struct WindowsDiskAccess;

impl DiskAccess for WindowsDiskAccess {
    fn open(&self, device: &Device, access: Access) -> Result<Box<dyn BlockDevice>> {
        let write = matches!(access, Access::ReadWriteExclusive);
        if write && device.read_only {
            return Err(Error::Refused(format!(
                "{} is write-protected",
                device.path
            )));
        }

        let number: u32 = device
            .id
            .strip_prefix("PhysicalDrive")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                Error::device(format!("cannot parse drive number from {}", device.id))
            })?;

        // Lock + dismount the disk's mounted volumes so we can write their sectors.
        let locked = if write {
            unsafe { lock_dismount_volumes(number) }
        } else {
            Vec::new()
        };

        let wpath = super::wide(&device.path);
        let access_flags = if write {
            GENERIC_READ.0 | GENERIC_WRITE.0
        } else {
            GENERIC_READ.0
        };
        let handle = match unsafe {
            CreateFileW(
                PCWSTR(wpath.as_ptr()),
                access_flags,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        } {
            Ok(h) => h,
            Err(e) => {
                unsafe { close_all(&locked) };
                return Err(Error::device(format!("cannot open {}: {e}", device.path)));
            }
        };

        if write {
            // Permit I/O beyond the last partition.
            let mut ret = 0u32;
            let _ = unsafe {
                DeviceIoControl(
                    handle,
                    FSCTL_ALLOW_EXTENDED_DASD_IO,
                    None,
                    0,
                    None,
                    0,
                    Some(&mut ret),
                    None,
                )
            };
        }

        Ok(Box::new(WindowsBlockDevice {
            handle,
            locked,
            sector_size: device.logical_sector_size.max(512),
            size: device.size,
            pos: 0,
        }))
    }
}

struct WindowsBlockDevice {
    handle: HANDLE,
    /// Locked+dismounted volume handles, kept open to hold the locks.
    locked: Vec<HANDLE>,
    sector_size: u32,
    size: u64,
    pos: u64,
}

// The handle is used from a single thread at a time (moved to the worker).
unsafe impl Send for WindowsBlockDevice {}

impl Read for WindowsBlockDevice {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut n = 0u32;
        unsafe { ReadFile(self.handle, Some(buf), Some(&mut n), None) }.map_err(win_err)?;
        self.pos += n as u64;
        Ok(n as usize)
    }
}

impl Write for WindowsBlockDevice {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut n = 0u32;
        unsafe { WriteFile(self.handle, Some(buf), Some(&mut n), None) }.map_err(win_err)?;
        self.pos += n as u64;
        Ok(n as usize)
    }
    fn flush(&mut self) -> io::Result<()> {
        unsafe { FlushFileBuffers(self.handle) }.map_err(win_err)
    }
}

impl Seek for WindowsBlockDevice {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(o) => o as i64,
            SeekFrom::End(o) => self.size as i64 + o,
            SeekFrom::Current(o) => self.pos as i64 + o,
        };
        if new < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative seek"));
        }
        unsafe { SetFilePointerEx(self.handle, new, None, FILE_BEGIN) }.map_err(win_err)?;
        self.pos = new as u64;
        Ok(self.pos)
    }
}

impl BlockDevice for WindowsBlockDevice {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }
    fn size(&self) -> u64 {
        self.size
    }
    fn sync(&mut self) -> Result<()> {
        unsafe { FlushFileBuffers(self.handle) }
            .map_err(|e| Error::Other(format!("FlushFileBuffers: {e}")))
    }
}

impl Drop for WindowsBlockDevice {
    fn drop(&mut self) {
        unsafe {
            close_all(&self.locked);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Open every drive letter, and for the volumes that live on `disk_number`
/// lock + dismount them, returning the (still-open, still-locked) handles.
unsafe fn lock_dismount_volumes(disk_number: u32) -> Vec<HANDLE> {
    let mut handles = Vec::new();
    for letter in b'A'..=b'Z' {
        let path = format!(r"\\.\{}:", letter as char);
        let w = super::wide(&path);
        let handle = match CreateFileW(
            PCWSTR(w.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        ) {
            Ok(h) => h,
            Err(_) => continue,
        };

        if volume_disk_number(handle) == Some(disk_number) {
            let mut ret = 0u32;
            let _ = DeviceIoControl(
                handle,
                FSCTL_LOCK_VOLUME,
                None,
                0,
                None,
                0,
                Some(&mut ret),
                None,
            );
            let _ = DeviceIoControl(
                handle,
                FSCTL_DISMOUNT_VOLUME,
                None,
                0,
                None,
                0,
                Some(&mut ret),
                None,
            );
            handles.push(handle);
        } else {
            let _ = CloseHandle(handle);
        }
    }
    handles
}

unsafe fn volume_disk_number(handle: HANDLE) -> Option<u32> {
    let mut num = STORAGE_DEVICE_NUMBER::default();
    let mut ret = 0u32;
    DeviceIoControl(
        handle,
        IOCTL_STORAGE_GET_DEVICE_NUMBER,
        None,
        0,
        Some(&mut num as *mut _ as *mut c_void),
        std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
        Some(&mut ret),
        None,
    )
    .ok()?;
    Some(num.DeviceNumber)
}

unsafe fn close_all(handles: &[HANDLE]) {
    for h in handles {
        let _ = CloseHandle(*h);
    }
}

fn win_err(e: windows::core::Error) -> io::Error {
    io::Error::other(e.to_string())
}
