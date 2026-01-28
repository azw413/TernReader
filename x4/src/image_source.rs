extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use core_io::{Read, Write};
use fatfs::{FileSystem, FsOptions};
use trusty_core::image_viewer::{EntryKind, ImageData, ImageEntry, ImageError, ImageSource};

use crate::sd_io::{detect_fat_partition, SdCardIo};

pub struct SdImageSource<D>
where
    D: embedded_sdmmc::BlockDevice,
    D::Error: core::fmt::Debug,
{
    sdcard: D,
}

impl<D> SdImageSource<D>
where
    D: embedded_sdmmc::BlockDevice,
    D::Error: core::fmt::Debug,
{
    pub fn new(sdcard: D) -> Self {
        Self { sdcard }
    }

    fn is_supported(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name.ends_with(".tri") || name.ends_with(".epub") || name.ends_with(".epb")
    }

    fn resume_filename() -> &'static str {
        ".trusty_resume"
    }

    fn open_fs(&self) -> Result<FileSystem<SdCardIo<'_, D>>, ImageError> {
        let base_lba = detect_fat_partition(&self.sdcard).map_err(|_| ImageError::Io)?;
        let io = SdCardIo::new(&self.sdcard, base_lba).map_err(|_| ImageError::Io)?;
        FileSystem::new(io, FsOptions::new()).map_err(|_| ImageError::Io)
    }

}

impl<D> ImageSource for SdImageSource<D>
where
    D: embedded_sdmmc::BlockDevice,
    D::Error: core::fmt::Debug,
{
    fn refresh(&mut self, path: &[String]) -> Result<Vec<ImageEntry>, ImageError> {
        let fs = self.open_fs()?;
        let mut read_dir = fs.root_dir();
        for part in path {
            read_dir = read_dir.open_dir(part).map_err(|_| ImageError::Io)?;
        }
        let mut entries = Vec::new();
        for entry in read_dir.iter() {
            let entry = entry.map_err(|_| ImageError::Io)?;
            let name = entry.file_name();
            if name.is_empty() || name == Self::resume_filename() || name == "." || name == ".." {
                continue;
            }
            if entry.is_dir() {
                entries.push(ImageEntry {
                    name,
                    kind: EntryKind::Dir,
                });
            } else if Self::is_supported(&name) {
                entries.push(ImageEntry {
                    name,
                    kind: EntryKind::File,
                });
            }
        }

        entries.sort_by(|a, b| match (a.kind, b.kind) {
            (EntryKind::Dir, EntryKind::File) => core::cmp::Ordering::Less,
            (EntryKind::File, EntryKind::Dir) => core::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });

        Ok(entries)
    }

    fn load(&mut self, path: &[String], entry: &ImageEntry) -> Result<ImageData, ImageError> {
        if entry.kind != EntryKind::File {
            return Err(ImageError::Message("Select a file, not a folder.".into()));
        }
        if entry
            .name
            .to_ascii_lowercase()
            .ends_with(".epub")
            || entry.name.to_ascii_lowercase().ends_with(".epb")
        {
            return Err(ImageError::Message("EPUB not implemented.".into()));
        }

        let fs = self.open_fs()?;
        let mut dir = fs.root_dir();
        for part in path {
            dir = dir.open_dir(part).map_err(|_| ImageError::Io)?;
        }
        let mut file = dir.open_file(&entry.name).map_err(|_| ImageError::Io)?;

        const MAX_IMAGE_BYTES: usize = 120_000;
        let mut file_len = None;
        for dir_entry in dir.iter() {
            let dir_entry = dir_entry.map_err(|_| ImageError::Io)?;
            if dir_entry.file_name() == entry.name {
                file_len = Some(dir_entry.len() as usize);
                break;
            }
        }
        let Some(file_len) = file_len else {
            return Err(ImageError::Io);
        };
        if file_len < 16 || file_len > MAX_IMAGE_BYTES {
            return Err(ImageError::Message(
                "Image size not supported on device.".into(),
            ));
        }

        let mut header = [0u8; 16];
        let read = file.read(&mut header).map_err(|_| ImageError::Io)?;
        if read != header.len() || &header[0..4] != b"TRIM" {
            return Err(ImageError::Unsupported);
        }
        if header[4] != 1 || header[5] != 1 {
            return Err(ImageError::Unsupported);
        }
        let width = u16::from_le_bytes([header[6], header[7]]) as u32;
        let height = u16::from_le_bytes([header[8], header[9]]) as u32;
        let expected = ((width as usize * height as usize) + 7) / 8;
        if 16 + expected != file_len {
            return Err(ImageError::Decode);
        }

        let mut bits = Vec::new();
        if bits.try_reserve(expected).is_err() {
            return Err(ImageError::Message(
                "Not enough memory for image buffer.".into(),
            ));
        }
        let mut buffer = [0u8; 512];
        while bits.len() < expected {
            let read = file.read(&mut buffer).map_err(|_| ImageError::Io)?;
            if read == 0 {
                break;
            }
            let remaining = expected - bits.len();
            let take = read.min(remaining);
            if bits.try_reserve(take).is_err() {
                return Err(ImageError::Message(
                    "Not enough memory while reading image.".into(),
                ));
            }
            bits.extend_from_slice(&buffer[..take]);
        }
        if bits.len() != expected {
            return Err(ImageError::Decode);
        }

        Ok(ImageData::Mono1 { width, height, bits })
    }

    fn save_resume(&mut self, name: Option<&str>) {
        let fs = match self.open_fs() {
            Ok(fs) => fs,
            Err(_) => return,
        };
        let root_dir = fs.root_dir();
        let resume_name = Self::resume_filename();
        if let Some(name) = name {
            let mut file = match root_dir.open_file(resume_name) {
                Ok(file) => file,
                Err(_) => match root_dir.create_file(resume_name) {
                    Ok(file) => file,
                    Err(_) => return,
                },
            };
            let _ = file.truncate();
            let _ = file.write(name.as_bytes());
        } else {
            let _ = root_dir.remove(resume_name);
        }
    }

    fn load_resume(&mut self) -> Option<String> {
        let fs = self.open_fs().ok()?;
        let mut file = fs.root_dir().open_file(Self::resume_filename()).ok()?;
        let mut buf = [0u8; 128];
        let read = file.read(&mut buf).ok()?;
        if read == 0 {
            return None;
        }
        let name = core::str::from_utf8(&buf[..read]).ok()?.trim();
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }
}
