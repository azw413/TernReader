extern crate alloc;

use alloc::string::ToString;
use alloc::vec::Vec;

use embedded_sdmmc::{Mode, VolumeIdx, VolumeManager};
use log::info;
use trusty_core::image_viewer::{ImageData, ImageEntry, ImageError, ImageSource};

pub struct SdImageSource<D, T, const MAX_DIRS: usize = 4, const MAX_FILES: usize = 4, const MAX_VOLUMES: usize = 1>
where
    D: embedded_sdmmc::BlockDevice,
    T: embedded_sdmmc::TimeSource,
    <D as embedded_sdmmc::BlockDevice>::Error: core::fmt::Debug,
{
    volume_mgr: VolumeManager<D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
}

impl<D, T, const MAX_DIRS: usize, const MAX_FILES: usize, const MAX_VOLUMES: usize>
    SdImageSource<D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>
where
    D: embedded_sdmmc::BlockDevice,
    T: embedded_sdmmc::TimeSource,
    <D as embedded_sdmmc::BlockDevice>::Error: core::fmt::Debug,
{
    pub fn new(volume_mgr: VolumeManager<D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>) -> Self {
        Self { volume_mgr }
    }

    fn is_supported(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name.ends_with(".png") || name.ends_with(".jpg") || name.ends_with(".jpeg")
    }

    fn decode_image_bytes(&self, _bytes: &[u8]) -> Result<ImageData, ImageError> {
        Err(ImageError::Message(
            "PNG/JPEG decoder not configured on device.".into(),
        ))
    }
}

impl<D, T, const MAX_DIRS: usize, const MAX_FILES: usize, const MAX_VOLUMES: usize> ImageSource
    for SdImageSource<D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>
where
    D: embedded_sdmmc::BlockDevice,
    T: embedded_sdmmc::TimeSource,
    <D as embedded_sdmmc::BlockDevice>::Error: core::fmt::Debug,
{
    fn refresh(&mut self) -> Result<Vec<ImageEntry>, ImageError> {
        let volume = self
            .volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(|_| ImageError::Io)?;
        let root_dir = volume.open_root_dir().map_err(|_| ImageError::Io)?;
        let images_dir = match root_dir.open_dir("IMAGES") {
            Ok(dir) => dir,
            Err(_) => {
                info!("No /images directory found.");
                return Ok(Vec::new());
            }
        };

        let mut entries = Vec::new();
        images_dir
            .iterate_dir(|entry| {
                if entry.attributes.is_directory() {
                    return;
                }
                let filename = entry.name.to_string();
                if Self::is_supported(&filename) {
                    entries.push(ImageEntry { name: filename });
                }
            })
            .map_err(|_| ImageError::Io)?;

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn load(&mut self, entry: &ImageEntry) -> Result<ImageData, ImageError> {
        let volume = self
            .volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(|_| ImageError::Io)?;
        let root_dir = volume.open_root_dir().map_err(|_| ImageError::Io)?;
        let images_dir = root_dir.open_dir("IMAGES").map_err(|_| ImageError::Io)?;
        let file = images_dir
            .open_file_in_dir(entry.name.as_str(), Mode::ReadOnly)
            .map_err(|_| ImageError::Io)?;

        let mut data = Vec::with_capacity(file.length() as usize);
        let mut buffer = [0u8; 512];
        while !file.is_eof() {
            let read = file.read(&mut buffer).map_err(|_| ImageError::Io)?;
            if read == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..read]);
        }

        self.decode_image_bytes(&data)
    }
}
