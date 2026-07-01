//! Physical-drive enumeration on Windows.

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BusTypeMmc, BusTypeNvme, BusTypeSata, BusTypeScsi, BusTypeSd, BusTypeUsb, CreateFileW,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, STORAGE_BUS_TYPE,
};
use windows::Win32::System::Ioctl::{
    PropertyStandardQuery, StorageDeviceProperty, GET_LENGTH_INFORMATION,
    IOCTL_DISK_GET_DRIVE_GEOMETRY, IOCTL_DISK_GET_LENGTH_INFO, IOCTL_STORAGE_QUERY_PROPERTY,
    STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
};
use windows::Win32::System::IO::DeviceIoControl;

use usbforge_core::device::{Bus, Device, DeviceEnumerator};
use usbforge_core::Result;

pub struct WindowsEnumerator;

impl DeviceEnumerator for WindowsEnumerator {
    fn list(&self, only_removable: bool) -> Result<Vec<Device>> {
        let mut devices = Vec::new();
        // Physical drive numbers are usually contiguous but can have gaps;
        // probe a reasonable range and keep whatever opens.
        for n in 0..32u32 {
            if let Some(dev) = probe_drive(n) {
                if !only_removable || dev.is_removable_media() {
                    devices.push(dev);
                }
            }
        }
        Ok(devices)
    }
}

fn probe_drive(n: u32) -> Option<Device> {
    let path = format!(r"\\.\PhysicalDrive{n}");
    let wpath = super::wide(&path);
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wpath.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .ok()?;

    let size = unsafe { disk_length(handle) }.unwrap_or(0);
    let sector = unsafe { sector_size(handle) }.unwrap_or(512);
    let (vendor, model, bus, removable) = unsafe { storage_descriptor(handle) };

    unsafe {
        let _ = CloseHandle(handle);
    }

    if size == 0 {
        return None;
    }
    Some(Device {
        id: format!("PhysicalDrive{n}"),
        path,
        vendor,
        model,
        serial: None,
        size,
        logical_sector_size: sector,
        bus,
        removable,
        read_only: false,
    })
}

unsafe fn disk_length(handle: HANDLE) -> Option<u64> {
    let mut info = GET_LENGTH_INFORMATION::default();
    let mut ret = 0u32;
    DeviceIoControl(
        handle,
        IOCTL_DISK_GET_LENGTH_INFO,
        None,
        0,
        Some(&mut info as *mut _ as *mut c_void),
        std::mem::size_of::<GET_LENGTH_INFORMATION>() as u32,
        Some(&mut ret),
        None,
    )
    .ok()?;
    Some(info.Length as u64)
}

unsafe fn sector_size(handle: HANDLE) -> Option<u32> {
    use windows::Win32::System::Ioctl::DISK_GEOMETRY;
    let mut geo = DISK_GEOMETRY::default();
    let mut ret = 0u32;
    DeviceIoControl(
        handle,
        IOCTL_DISK_GET_DRIVE_GEOMETRY,
        None,
        0,
        Some(&mut geo as *mut _ as *mut c_void),
        std::mem::size_of::<DISK_GEOMETRY>() as u32,
        Some(&mut ret),
        None,
    )
    .ok()?;
    Some(geo.BytesPerSector)
}

/// Returns `(vendor, product, bus, removable)`.
unsafe fn storage_descriptor(handle: HANDLE) -> (String, String, Bus, bool) {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut buf = [0u8; 1024];
    let mut ret = 0u32;
    let ok = DeviceIoControl(
        handle,
        IOCTL_STORAGE_QUERY_PROPERTY,
        Some(&query as *const _ as *const c_void),
        std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
        Some(buf.as_mut_ptr() as *mut c_void),
        buf.len() as u32,
        Some(&mut ret),
        None,
    )
    .is_ok();

    if !ok || (ret as usize) < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
        return (String::new(), String::new(), Bus::Unknown, false);
    }

    let desc = &*(buf.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR);
    let vendor = ascii_at(&buf, desc.VendorIdOffset as usize);
    let product = ascii_at(&buf, desc.ProductIdOffset as usize);
    let bus = map_bus(desc.BusType);
    (vendor, product, bus, desc.RemovableMedia)
}

/// Read a NUL-terminated ASCII string at `offset` in `buf`, trimmed.
fn ascii_at(buf: &[u8], offset: usize) -> String {
    if offset == 0 || offset >= buf.len() {
        return String::new();
    }
    let bytes = &buf[offset..];
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

fn map_bus(bus: STORAGE_BUS_TYPE) -> Bus {
    // Compare (not match) — the windows-rs constants are camelCase, which trips
    // the `non_upper_case_globals` lint when used as match patterns.
    if bus == BusTypeUsb {
        Bus::Usb
    } else if bus == BusTypeSata {
        Bus::Sata
    } else if bus == BusTypeNvme {
        Bus::Nvme
    } else if bus == BusTypeScsi {
        Bus::Scsi
    } else if bus == BusTypeSd || bus == BusTypeMmc {
        Bus::Mmc
    } else {
        Bus::Unknown
    }
}
