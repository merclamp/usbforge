//! Source-image inspection.
//!
//! Eventually this module grows the ISO9660/UDF reader and the bootability
//! analysis that Rufus does in `iso.c` + `ImageScanThread`. For now it provides
//! lightweight detection so the CLI/GUI can describe a selected source.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    /// ISO9660 / UDF optical image.
    Iso,
    /// Raw disk image (`.img`, `.raw`, `.dd`) written sector-for-sector.
    RawDisk,
    /// Compressed disk image (`.gz`, `.xz`, `.zst`, ...) expanded on write.
    CompressedDisk,
    /// Microsoft VHD/VHDX virtual disk.
    Vhd,
    Unknown,
}

impl ImageKind {
    pub fn label(self) -> &'static str {
        match self {
            ImageKind::Iso => "ISO image",
            ImageKind::RawDisk => "raw disk image",
            ImageKind::CompressedDisk => "compressed disk image",
            ImageKind::Vhd => "VHD/VHDX",
            ImageKind::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImageInfo {
    pub path: PathBuf,
    pub size: u64,
    pub kind: ImageKind,
}

impl ImageInfo {
    /// Inspect a file on disk. Classification is by extension for now; magic-byte
    /// sniffing and ISO parsing land with the image-writer milestone.
    pub fn inspect(path: impl AsRef<Path>) -> crate::Result<ImageInfo> {
        let path = path.as_ref();
        let size = std::fs::metadata(path)?.len();
        let kind = classify_by_extension(path);
        Ok(ImageInfo {
            path: path.to_path_buf(),
            size,
            kind,
        })
    }
}

fn classify_by_extension(path: &Path) -> ImageKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "iso" => ImageKind::Iso,
        "img" | "raw" | "dd" | "bin" => ImageKind::RawDisk,
        "gz" | "xz" | "zst" | "zstd" | "bz2" | "lz4" | "lzma" | "z" => ImageKind::CompressedDisk,
        "vhd" | "vhdx" => ImageKind::Vhd,
        _ => ImageKind::Unknown,
    }
}
