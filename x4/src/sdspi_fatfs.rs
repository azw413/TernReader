use core::{
    ffi::c_void,
    fmt::{Error, Write},
};

use alloc::{slice, vec::Vec};
use alloc::string::ToString;
use embedded_hal_bus::spi::RefCellDevice;
use embedded_io::{ErrorType, Read, Seek, SeekFrom};
use embedded_sdmmc::{Block, BlockDevice, BlockIdx, SdCard, sdcard};
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use esp_hal::spi::master::Spi;
use log::trace;
use tern_core::fs::{Filesystem, Mode};
use crate::sdspi_fs::UsbFsOps;

pub type BYTE = u8;
pub type WORD = u16;
pub type DWORD = u32;
pub type QWORD = u64;
pub type UINT = u32;
pub type Sector = u32;
pub type DSTATUS = u8;
pub const STA_NOINIT: DSTATUS = 0x01; /* Drive not initialized */
pub const STA_NODISK: DSTATUS = 0x02; /* No medium in the drive */
pub const STA_PROTECT: DSTATUS = 0x04; /* Write protected */

pub const SECTOR_SIZE: usize = 512;

pub type DRESULT = u32;
pub const DRESULT_RES_OK: DRESULT = 0;
pub const DRESULT_RES_ERROR: DRESULT = 1;
pub const DRESULT_RES_WRPRT: DRESULT = 2;
pub const DRESULT_RES_NOTRDY: DRESULT = 3;
pub const DRESULT_RES_PARERR: DRESULT = 4;

/* Generic command (Used by FatFs) */
const CTRL_SYNC: BYTE = 0; /* Complete pending write process (needed at FF_FS_READONLY == 0) */
const GET_SECTOR_COUNT: BYTE = 1; /* Get media size (needed at FF_USE_MKFS == 1) */
const GET_SECTOR_SIZE: BYTE = 2; /* Get sector size (needed at FF_MAX_SS != FF_MIN_SS) */
const GET_BLOCK_SIZE: BYTE = 3; /* Get erase block size (needed at FF_USE_MKFS == 1) */
const CTRL_TRIM: BYTE = 4; /* Inform device that the data on the block of sectors is no longer used (needed at FF_USE_TRIM == 1) */

// FRESULT as newtype to implement traits
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FRESULT(pub i32);

impl FRESULT {
    pub const OK: FRESULT = FRESULT(0);
}

// Implement embedded_io::Error for FRESULT
impl core::error::Error for FRESULT {}

impl core::fmt::Display for FRESULT {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "FatFs error: {}", self.0)
    }
}

impl core::fmt::Debug for FRESULT {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "FRESULT({})", self.0)
    }
}

impl embedded_io::Error for FRESULT {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

type SPI = RefCellDevice<'static, Spi<'static, Blocking>, Output<'static>, Delay>;
type Sd = SdCard<SPI, Delay>;

static mut DRIVER: Option<Sd> = None;

pub fn open(spi: SPI, delay: Delay) {
    let sd = SdCard::new(spi, delay);
    unsafe {
        DRIVER = Some(sd);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn disk_initialize(_pdrv: BYTE) -> DSTATUS {
    trace!("disk_initialize called");
    unsafe { disk_status(_pdrv) }
    // STA_NODISK
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn disk_status(_pdrv: BYTE) -> DSTATUS {
    trace!("disk_status called");
    unsafe {
        if let Some(driver) = &*core::ptr::addr_of!(DRIVER) {
            match driver.num_bytes() {
                Ok(_) => 0, // Return 0 for initialized
                Err(ex) => {
                    log::error!("Disk num bytes error: {:?}", ex);
                    STA_NOINIT // Return not initialized status
                }
            }
        } else {
            log::error!("Disk driver not set");
            STA_NOINIT // Return not initialized status if driver is None
        }
    }
    // STA_NODISK
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn disk_read(
    _pdrv: BYTE,
    buff: *mut BYTE,
    sector: Sector,
    count: UINT,
) -> DRESULT {
    trace!("disk_read called: sector {}, count {}", sector, count);
    unsafe {
        if let Some(driver) = &*core::ptr::addr_of!(DRIVER) {
            for i in 0..count {
                let mut block = [Block::new()];
                let block_idx = BlockIdx((sector + i) as _);
                if let Err(_) = driver.read(&mut block, block_idx) {
                    return DRESULT_RES_ERROR;
                }
                let block_bytes = block[0].as_slice();
                let dest = core::slice::from_raw_parts_mut(
                    buff.add((i as usize) * SECTOR_SIZE),
                    SECTOR_SIZE,
                );
                dest.copy_from_slice(block_bytes);
            }
            DRESULT_RES_OK
        } else {
            DRESULT_RES_NOTRDY
        }
    }
    // DRESULT_RES_NOTRDY
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn disk_write(
    _pdrv: BYTE,
    buff: *const BYTE,
    sector: Sector,
    count: UINT,
) -> DRESULT {
    trace!("disk_write called: sector {}, count {}", sector, count);
    unsafe {
        if let Some(driver) = &*core::ptr::addr_of!(DRIVER) {
            for i in 0..count {
                let mut block = [Block::new()];
                let block_idx = BlockIdx((sector + i) as _);
                let src = slice::from_raw_parts(buff.add((i as usize) * SECTOR_SIZE), SECTOR_SIZE);
                block[0].as_mut_slice().copy_from_slice(src);
                if let Err(_) = driver.write(&block, block_idx) {
                    return DRESULT_RES_ERROR;
                }
            }
            DRESULT_RES_OK
        } else {
            DRESULT_RES_NOTRDY
        }
    }
    // DRESULT_RES_NOTRDY
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn disk_ioctl(_lun: BYTE, _cmd: BYTE, _buff: *mut c_void) -> DRESULT {
    trace!("disk_ioctl called");
    unsafe {
        if let Some(driver) = &*core::ptr::addr_of!(DRIVER) {
            match _cmd {
                CTRL_SYNC => {
                    // No cache to flush; treat as OK.
                    DRESULT_RES_OK
                }
                GET_SECTOR_COUNT => {
                    if !_buff.is_null() {
                        if let Ok(bytes) = driver.num_bytes() {
                            let sectors = (bytes as u64 / SECTOR_SIZE as u64) as DWORD;
                            *(_buff as *mut DWORD) = sectors;
                            return DRESULT_RES_OK;
                        }
                    }
                    DRESULT_RES_ERROR
                }
                GET_SECTOR_SIZE => {
                    if !_buff.is_null() {
                        *(_buff as *mut WORD) = SECTOR_SIZE as WORD;
                        return DRESULT_RES_OK;
                    }
                    DRESULT_RES_ERROR
                }
                GET_BLOCK_SIZE => {
                    if !_buff.is_null() {
                        *(_buff as *mut DWORD) = 1;
                        return DRESULT_RES_OK;
                    }
                    DRESULT_RES_ERROR
                }
                _ => DRESULT_RES_PARERR,
            }
        } else {
            DRESULT_RES_NOTRDY
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_fattime() -> DWORD {
    trace!("get_fattime called");
    0
}

// FFOBJID structure from ff.h
// This is the object identifier used by FIL and DIR
#[repr(C)]
#[derive(Clone, Copy)]
struct FFOBJID {
    fs: *mut FATFS,
    id: WORD,
    attr: BYTE,
    stat: BYTE,
    sclust: DWORD,
    objsize: QWORD, // FSIZE_t (using DWORD since FF_FS_EXFAT is 0)
    n_cont: DWORD,
    n_frag: DWORD,
    c_scl: DWORD,
    c_size: DWORD,
    c_ofs: DWORD,
}

// FATFS structure (incomplete, just for pointer)
#[repr(C)]
struct FATFS {
    _private: [u8; 0],
}

// FIL structure from ff.h
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FIL {
    obj: FFOBJID,
    flag: BYTE,
    err: BYTE,
    fptr: QWORD, // FSIZE_t
    clust: DWORD,
    sect: DWORD,        // LBA_t
    dir_sect: DWORD,    // LBA_t (only if !FF_FS_READONLY)
    dir_ptr: *mut BYTE, // (only if !FF_FS_READONLY)
    buf: [BYTE; 512],   // FF_MAX_SS (only if !FF_FS_TINY)
}

// DIR structure from ff.h
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DIR {
    obj: FFOBJID,
    dptr: DWORD,
    clust: DWORD,
    sect: DWORD, // LBA_t
    dir: *mut BYTE,
    fn_: [BYTE; 12],
    blk_ofs: DWORD, // (only if FF_USE_LFN)
}

// FILINFO structure from ff.h
#[repr(C)]
pub struct FILINFO {
    fsize: QWORD, // FSIZE_t
    fdate: WORD,
    ftime: WORD,
    fattrib: BYTE,
    altname: [u8; 13], // FF_SFN_BUF + 1 (12 + 1)
    fname: [u8; 256],  // FF_LFN_BUF + 1 (255 + 1)
}

const _: () = assert!(
    core::mem::size_of::<FFOBJID>() == 48,
    "FFOBJID size must be 0 bytes as it's opaque"
);
const _: () = assert!(
    core::mem::size_of::<FIL>() == 592,
    "FIL size must be 588 bytes to match C"
);
const _: () = assert!(
    core::mem::size_of::<DIR>() == 80,
    "DIR size must be 64 bytes to match C"
);
const _: () = assert!(
    core::mem::size_of::<FILINFO>() == 288,
    "FILINFO size must be 320 bytes to match C"
);

unsafe extern "C" {
    // Core file operations
    fn f_open(fp: *mut FIL, path: *const u8, mode: BYTE) -> FRESULT;
    fn f_close(fp: *mut FIL) -> FRESULT;
    fn f_read(fp: *mut FIL, buff: *mut u8, btr: UINT, br: *mut UINT) -> FRESULT;
    fn f_write(fp: *mut FIL, buff: *const u8, btw: UINT, bw: *mut UINT) -> FRESULT;
    fn f_lseek(fp: *mut FIL, ofs: QWORD) -> FRESULT; // FSIZE_t = QWORD
    fn f_truncate(fp: *mut FIL) -> FRESULT;
    fn f_sync(fp: *mut FIL) -> FRESULT;

    // Directory operations
    fn f_opendir(dp: *mut DIR, path: *const u8) -> FRESULT;
    fn f_closedir(dp: *mut DIR) -> FRESULT;
    fn f_readdir(dp: *mut DIR, fno: *mut FILINFO) -> FRESULT;

    // File/directory management
    fn f_mkdir(path: *const u8) -> FRESULT;
    fn f_unlink(path: *const u8) -> FRESULT;
    fn f_rename(path_old: *const u8, path_new: *const u8) -> FRESULT;
    fn f_stat(path: *const u8, fno: *mut FILINFO) -> FRESULT;

    // Volume operations
    fn f_mount(fs: *mut FATFS, path: *const u8, opt: BYTE) -> FRESULT;
    fn f_chdir(path: *const u8) -> FRESULT;
    fn f_chdrive(path: *const u8) -> FRESULT;
    fn f_getcwd(buff: *mut u8, len: UINT) -> FRESULT;

    // Custom helper functions
    fn ff_mount() -> FRESULT;
    fn ff_exists(path: *const u8) -> bool;
    fn getnum() -> i32;
}

pub struct FatFs;

impl FatFs {
    pub fn new(spi: SPI, delay: Delay) -> Self {
        let sd = SdCard::new(spi, delay);
        unsafe {
            DRIVER = Some(sd);
            ff_mount();
        }
        FatFs
    }
}

fn null_terminate(path: &str) -> [u8; 512] {
    assert!(
        path.len() < 512,
        "Path too long for static null-terminated buffer"
    );
    let mut null_terminated_path = [0u8; 512];
    null_terminated_path[..path.len()].copy_from_slice(path.as_bytes());
    null_terminated_path
}

impl ErrorType for FatFs {
    type Error = FRESULT;
}

impl Filesystem for FatFs {
    type Directory<'a> = DirectoryEntry
    where
        Self: 'a;
    type File<'a> = FileEntry
    where
        Self: 'a;
    fn open_file(
        &self,
        path: &str,
        mode: Mode,
    ) -> Result<Self::File<'_>, Self::Error> {
        let path = null_terminate(path);
        let mode = match mode {
            Mode::Read => 0x01,                    // FA_READ
            Mode::Write => 0x02 | 0x08,            // FA_WRITE | FA_CREATE_ALWAYS
            Mode::ReadWrite => 0x01 | 0x02 | 0x10, // FA_READ | FA_WRITE | FA_OPEN_ALWAYS
        };
        unsafe {
            let mut f: FIL = core::mem::zeroed();
            let res = f_open(&mut f as *mut FIL, path.as_ptr(), mode);
            if res.0 != 0 {
                Err(res)
            } else {
                Ok(FileEntry { f })
            }
        }
    }
    fn create_dir_all(&self, path: &str) -> Result<(), Self::Error> {
        let path = null_terminate(path);
        let res = unsafe { f_mkdir(path.as_ptr()) };
        if res.0 != 0 { Err(res) } else { Ok(()) }
    }
    fn exists(&self, path: &str) -> Result<bool, Self::Error> {
        let path = null_terminate(path);
        Ok(unsafe { ff_exists(path.as_ptr()) })
    }
    fn open_directory(&self, path: &str) -> Result<Self::Directory<'_>, Self::Error> {
        let path = null_terminate(path);
        unsafe {
            let mut d: DIR = core::mem::zeroed();
            let res = f_opendir(&mut d as *mut DIR, path.as_ptr());
            if res.0 != 0 {
                Err(res)
            } else {
                Ok(DirectoryEntry { d })
            }
        }
    }
    fn open_file_entry(
        &self,
        _dir: &Self::Directory<'_>,
        entry: &<<Self as Filesystem>::Directory<'_> as tern_core::fs::Directory>::Entry,
        mode: Mode,
    ) -> Result<Self::File<'_>, Self::Error> {
        // Build path from directory and entry name
        let name = &entry.name;
        self.open_file(name, mode)
    }
}

impl UsbFsOps for FatFs {
    fn delete_file(&self, path: &str) -> Result<(), embedded_sdmmc::Error<sdcard::Error>> {
        let path = null_terminate(path);
        let res = unsafe { f_unlink(path.as_ptr()) };
        if res.0 != 0 {
            Err(embedded_sdmmc::Error::DeviceError(sdcard::Error::WriteError))
        } else {
            Ok(())
        }
    }

    fn rename_file(&self, from: &str, to: &str) -> Result<(), embedded_sdmmc::Error<sdcard::Error>> {
        let from = null_terminate(from);
        let to = null_terminate(to);
        let res = unsafe { f_rename(from.as_ptr(), to.as_ptr()) };
        if res.0 != 0 {
            Err(embedded_sdmmc::Error::DeviceError(sdcard::Error::WriteError))
        } else {
            Ok(())
        }
    }
}

pub struct DirEntry {
    name: alloc::string::String,
    size: usize,
    is_dir: bool,
}

impl DirEntry {
    fn from_filinfo(fno: &FILINFO) -> Self {
        // Find the null terminator in fname
        let name_bytes = fno
            .fname
            .iter()
            .take_while(|&&b| b != 0)
            .copied()
            .collect::<Vec<u8>>();
        let raw = alloc::string::String::from_utf8_lossy(&name_bytes).into_owned();
        let name = if raw.contains('.') {
            raw.trim().to_string()
        } else if raw.len() > 8 {
            let base = raw.chars().take(8).collect::<alloc::string::String>();
            let ext = raw.chars().skip(8).take(3).collect::<alloc::string::String>();
            let base = base.trim().to_string();
            let ext = ext.trim().to_string();
            if ext.is_empty() {
                base
            } else {
                alloc::format!("{}.{}", base, ext)
            }
        } else {
            raw.trim().to_string()
        };

        let is_dir = (fno.fattrib & 0x10) != 0; // AM_DIR = 0x10
        let size = fno.fsize as usize;

        Self { name, size, is_dir }
    }
}

impl tern_core::fs::DirEntry for DirEntry {
    fn is_directory(&self) -> bool {
        self.is_dir
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn size(&self) -> usize {
        self.size
    }
}

pub struct DirectoryEntry {
    d: DIR,
}

impl Drop for DirectoryEntry {
    fn drop(&mut self) {
        unsafe {
            f_closedir(&mut self.d as *mut DIR);
        }
    }
}

impl ErrorType for DirectoryEntry {
    type Error = FRESULT;
}

impl tern_core::fs::Directory for DirectoryEntry {
    type Entry = DirEntry;

    fn list(&self) -> Result<Vec<Self::Entry>, Self::Error> {
        let mut entries = Vec::new();
        unsafe {
            // Need to create a mutable copy to iterate
            let mut d = self.d;
            loop {
                let mut fno: FILINFO = core::mem::zeroed();
                let res = f_readdir(&mut d as *mut DIR, &mut fno as *mut FILINFO);
                if res.0 != 0 {
                    return Err(res);
                }
                // Empty name means end of directory
                if fno.fname[0] == 0 {
                    break;
                }
                entries.push(DirEntry::from_filinfo(&fno));
            }
        }
        Ok(entries)
    }
}

pub struct FileEntry {
    f: FIL,
}

impl ErrorType for FileEntry {
    type Error = FRESULT;
}

impl Read for FileEntry {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut bytes_read: UINT = 0;
        let res = unsafe {
            f_read(
                &mut self.f as *mut FIL,
                buf.as_mut_ptr(),
                buf.len() as UINT,
                &mut bytes_read as *mut UINT,
            )
        };
        if res.0 != 0 {
            Err(res)
        } else {
            Ok(bytes_read as usize)
        }
    }
}

impl Write for FileEntry {
    fn write_str(&mut self, s: &str) -> Result<(), Error> {
        let mut bytes_written: UINT = 0;
        let res = unsafe {
            f_write(
                &mut self.f as *mut FIL,
                s.as_ptr(),
                s.len() as UINT,
                &mut bytes_written as *mut UINT,
            )
        };
        if res.0 != 0 { Err(Error) } else { Ok(()) }
    }
}

impl embedded_io::Write for FileEntry {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut bytes_written: UINT = 0;
        let res = unsafe {
            f_write(
                &mut self.f as *mut FIL,
                buf.as_ptr(),
                buf.len() as UINT,
                &mut bytes_written as *mut UINT,
            )
        };
        if res.0 != 0 {
            Err(res)
        } else {
            Ok(bytes_written as usize)
        }
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        let res = unsafe { f_sync(&mut self.f as *mut FIL) };
        if res.0 != 0 { Err(res) } else { Ok(()) }
    }
}

impl Seek for FileEntry {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let new_pos = match pos {
            SeekFrom::Start(offset) => offset,
            SeekFrom::End(offset) => (self.f.obj.objsize as i64 + offset) as u64,
            SeekFrom::Current(offset) => (self.f.fptr as i64 + offset) as u64,
        };

        let res = unsafe { f_lseek(&mut self.f as *mut FIL, new_pos) };

        if res.0 != 0 {
            Err(res)
        } else {
            Ok(self.f.fptr)
        }
    }
}

impl tern_core::fs::File for FileEntry {
    fn size(&self) -> usize {
        self.f.obj.objsize as usize
    }
}
