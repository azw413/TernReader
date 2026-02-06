use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use embedded_io::{ErrorType, SeekFrom};
use embedded_sdmmc::{sdcard, LfnBuffer, RawVolume, SdCard, VolumeManager};
use esp_hal::delay::Delay;
use tern_core::fs::{DirEntry, Mode};

/// Dummy time source for embedded-sdmmc (RTC requires too much power)
pub struct DummyTimeSource;

impl embedded_sdmmc::TimeSource for DummyTimeSource {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp {
            year_since_1970: 0,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

pub struct SdSpiFilesystem<SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    volume_mgr: VolumeManager<SdCard<SPI, Delay>, DummyTimeSource>,
    volume: RawVolume,
}

type Error = embedded_sdmmc::Error<sdcard::Error>;
type Result<T> = core::result::Result<T, Error>;

impl<SPI> ErrorType for SdSpiFilesystem<SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    type Error = Error;
}

impl<SPI> SdSpiFilesystem<SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    pub fn new_with_volume(spi: SPI, delay: Delay) -> Result<Self> {
        let sdcard = SdCard::new(spi, delay);
        let volume_mgr = VolumeManager::new(sdcard, DummyTimeSource);
        let volume = volume_mgr.open_raw_volume(embedded_sdmmc::VolumeIdx(0))?;
        Ok(SdSpiFilesystem { volume_mgr, volume })
    }

    fn components(path: &str) -> impl Iterator<Item = &str> {
        path.split('/').filter(|s| !s.is_empty())
    }

    fn find_entry_in_dir(
        dir: &mut embedded_sdmmc::Directory<'_, SdCard<SPI, Delay>, DummyTimeSource, 4, 4, 1>,
        name: &str,
    ) -> Result<embedded_sdmmc::DirEntry> {
        if let Ok(entry) = dir.find_directory_entry(name) {
            return Ok(entry);
        }
        log::debug!("SD find entry: '{}'", name);
        let mut entries: Option<embedded_sdmmc::DirEntry> = None;
        let mut buffer = [0u8; 256];
        let mut lfn = LfnBuffer::new(&mut buffer);
        dir.iterate_dir_lfn(&mut lfn, |entry, lfn| {
            if entries.is_some() {
                return;
            }
            if let Some(lfn_name) = lfn {
                let candidate = lfn_name.to_string();
                log::debug!("SD entry LFN: {}", candidate);
                if candidate.trim().eq_ignore_ascii_case(name) {
                    entries = Some(entry.clone());
                    return;
                }
            }
            let candidate = entry.name.to_string();
            log::debug!("SD entry short: {}", candidate);
            if candidate.trim().eq_ignore_ascii_case(name) {
                entries = Some(entry.clone());
            }
        })?;
        if let Some(entry) = entries {
            return Ok(entry);
        }
        // Fallback: let embedded-sdmmc do a short-name lookup.
        log::warn!("SD entry not found via scan: {}", name);
        match dir.find_directory_entry(name) {
            Ok(entry) => Ok(entry),
            Err(err) => {
                log::warn!("SD entry lookup failed for '{}': {:?}", name, err);
                Err(err)
            }
        }
    }
}

impl<SPI> tern_core::fs::Filesystem for SdSpiFilesystem<SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    type File<'a>
        = SdSpiFile<'a, SPI>
    where
        Self: 'a;
    type Directory<'a>
        = SdSpiDirectory<'a, SPI>
    where
        Self: 'a;

    fn create_dir_all(&self, path: &str) -> Result<()> {
        let raw_root = self.volume_mgr.open_root_dir(self.volume)?;
        let mut dir = raw_root.to_directory(&self.volume_mgr);

        for comp in Self::components(path) {
            let _ = dir.make_dir_in_dir(comp);
            dir.change_dir(comp)?;
        }

        Ok(())
    }

    fn exists(&self, path: &str) -> Result<bool> {
        let raw_root = self.volume_mgr.open_root_dir(self.volume)?;
        let mut dir = raw_root.to_directory(&self.volume_mgr);
        let mut components = Self::components(path).peekable();
        while let Some(comp) = components.next() {
            let entry = Self::find_entry_in_dir(&mut dir, comp)?;
            if !entry.attributes.is_directory() {
                return Ok(components.peek().is_none());
            }
            if components.peek().is_some() {
                dir.change_dir(entry.name)?;
            }
        }
        Ok(true)
    }

    fn open_file(&self, path: &str, mode: Mode) -> Result<Self::File<'_>> {
        log::debug!("SD open file: '{}'", path);
        let raw_root = self.volume_mgr.open_root_dir(self.volume)?;
        let mut dir = raw_root.to_directory(&self.volume_mgr);
        let mut components = Self::components(path.trim_start_matches('/')).peekable();
        while let Some(comp) = components.next() {
            let is_last = components.peek().is_none();
            let entry = match Self::find_entry_in_dir(&mut dir, comp) {
                Ok(entry) => Some(entry),
                Err(err) => {
                    if is_last && !matches!(mode, Mode::Read) {
                        let mode = match mode {
                            Mode::Read => embedded_sdmmc::Mode::ReadOnly,
                            Mode::Write => embedded_sdmmc::Mode::ReadWriteCreateOrTruncate,
                            Mode::ReadWrite => embedded_sdmmc::Mode::ReadWriteAppend,
                        };
                        let file = dir.open_file_in_dir(comp, mode)?;
                        let raw_file = file.to_raw_file();
                        let file = embedded_sdmmc::File::new(raw_file, &self.volume_mgr);
                        let size = file.length();
                        return Ok(SdSpiFile { file, size });
                    }
                    return Err(err);
                }
            };
            if let Some(entry) = entry {
                if !entry.attributes.is_directory() {
                    if !is_last {
                        return Err(Error::NotFound);
                    }
                    let size = entry.size;
                    let mode = match mode {
                        Mode::Read => embedded_sdmmc::Mode::ReadOnly,
                        Mode::Write => embedded_sdmmc::Mode::ReadWriteCreateOrTruncate,
                        Mode::ReadWrite => embedded_sdmmc::Mode::ReadWriteAppend,
                    };
                    let file = dir.open_file_in_dir(entry.name, mode)?;
                    let raw_file = file.to_raw_file();
                    return Ok(SdSpiFile {
                        file: embedded_sdmmc::File::new(raw_file, &self.volume_mgr),
                        size,
                    });
                }
                if !is_last {
                    dir.change_dir(entry.name)?;
                }
            }
        }
        Err(Error::NotFound)
    }

    fn open_directory(&self, path: &str) -> Result<Self::Directory<'_>> {
        log::debug!("SD open directory: '{}'", path);
        let raw_root = self.volume_mgr.open_root_dir(self.volume)?;
        let mut dir = raw_root.to_directory(&self.volume_mgr);
        let mut components = Self::components(path.trim_start_matches('/'));
        while let Some(comp) = components.next() {
            let entry = Self::find_entry_in_dir(&mut dir, comp)?;
            dir.change_dir(entry.name)?;
        }
        let raw_dir = dir.to_raw_directory();
        Ok(SdSpiDirectory {
            dir: raw_dir.to_directory(&self.volume_mgr),
        })
    }

    fn open_file_entry(
        &self,
        dir: &Self::Directory<'_>,
        entry: &SdSpiDirEntry,
        mode: Mode,
    ) -> Result<Self::File<'_>> {
        if entry.is_directory() {
            return Err(Error::OpenedDirAsFile);
        }

        let size = entry.size() as u32;
        let mode = match mode {
            Mode::Read => embedded_sdmmc::Mode::ReadOnly,
            Mode::Write => embedded_sdmmc::Mode::ReadWriteCreateOrTruncate,
            Mode::ReadWrite => embedded_sdmmc::Mode::ReadWriteAppend,
        };
        let file = dir.dir.open_file_in_dir(entry.name(), mode)?;
        let raw_file = file.to_raw_file();
        Ok(SdSpiFile {
            file: raw_file.to_file(&self.volume_mgr),
            size,
        })
    }
}

pub struct SdSpiFile<'a, SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    file: embedded_sdmmc::File<'a, SdCard<SPI, Delay>, DummyTimeSource, 4, 4, 1>,
    size: u32,
}

impl<SPI> ErrorType for SdSpiFile<'_, SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    type Error = embedded_sdmmc::Error<sdcard::Error>;
}

impl<'a, SPI> tern_core::fs::File for SdSpiFile<'a, SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    fn size(&self) -> usize {
        self.size as usize
    }
}

impl<SPI> embedded_io::Seek for SdSpiFile<'_, SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    fn seek(&mut self, pos: SeekFrom) -> core::result::Result<u64, Self::Error> {
        match pos {
            SeekFrom::Current(off) => self.file.seek_from_current(off as _),
            SeekFrom::End(off) => self.file.seek_from_end(off as _),
            SeekFrom::Start(pos) => self.file.seek_from_start(pos as _),
        }?;
        Ok(self.file.offset() as u64)
    }
}

impl<SPI> embedded_io::Read for SdSpiFile<'_, SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    fn read(&mut self, buf: &mut [u8]) -> core::result::Result<usize, Self::Error> {
        self.file.read(buf)
    }
}

impl<SPI> embedded_io::Write for SdSpiFile<'_, SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    fn write(&mut self, buf: &[u8]) -> core::result::Result<usize, Self::Error> {
        self.file.write(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> core::result::Result<(), Self::Error> {
        self.file.flush()
    }
}

pub struct SdSpiDirectory<'a, SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    dir: embedded_sdmmc::Directory<'a, SdCard<SPI, Delay>, DummyTimeSource, 4, 4, 1>,
}

impl<SPI> ErrorType for SdSpiDirectory<'_, SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    type Error = embedded_sdmmc::Error<sdcard::Error>;
}

impl<SPI> tern_core::fs::Directory for SdSpiDirectory<'_, SPI>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
{
    type Entry = SdSpiDirEntry;

    fn list(&self) -> Result<Vec<Self::Entry>> {
        let mut entries = Vec::new();
        let mut buffer = [0u8; 256];
        let mut lfn = LfnBuffer::new(&mut buffer);
        self.dir.iterate_dir_lfn(&mut lfn, |entry, lfn| {
            let name = lfn
                .map(|lfn| lfn.to_string())
                .unwrap_or(entry.name.to_string());
            let short_name = entry.name.to_string();
            entries.push(SdSpiDirEntry {
                entry: entry.clone(),
                name,
                short_name,
            });
        })?;
        Ok(entries)
    }
}

pub struct SdSpiDirEntry {
    pub entry: embedded_sdmmc::DirEntry,
    pub name: String,
    pub short_name: String,
}

impl tern_core::fs::DirEntry for SdSpiDirEntry {
    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn short_name(&self) -> &str {
        self.short_name.as_str()
    }

    fn is_directory(&self) -> bool {
        self.entry.attributes.is_directory()
    }

    fn size(&self) -> usize {
        self.entry.size as usize
    }
}
