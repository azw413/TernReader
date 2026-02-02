extern crate alloc;

use core::result::Result;

use alloc::vec::Vec;
use embedded_io::{ErrorType, Read, Seek, Write};

pub enum Mode {
    Read,
    Write,
    ReadWrite,
}

pub trait Filesystem: ErrorType {
    type File<'a>: File
    where
        Self: 'a;
    type Directory<'a>: Directory
    where
        Self: 'a;

    fn open_file(&self, path: &str, mode: Mode) -> Result<Self::File<'_>, Self::Error>;
    fn open_file_entry(
        &self,
        dir: &Self::Directory<'_>,
        entry: &<<Self as Filesystem>::Directory<'_> as Directory>::Entry,
        mode: Mode,
    ) -> Result<Self::File<'_>, Self::Error>;
    fn open_directory(&self, path: &str) -> Result<Self::Directory<'_>, Self::Error>;
    fn exists(&self, path: &str) -> Result<bool, Self::Error>;
    fn create_dir_all(&self, path: &str) -> Result<(), Self::Error>;
}

pub trait File: Read + Write + Seek {
    fn size(&self) -> usize;
    unsafe fn read_sized<T: Sized>(&mut self) -> core::result::Result<T, Self::Error> {
        let mut value: T = unsafe { core::mem::zeroed() };
        let buf = unsafe {
            core::slice::from_raw_parts_mut(
                &mut value as *mut T as *mut u8,
                core::mem::size_of::<T>(),
            )
        };
        self.read(buf)?;
        Ok(value)
    }
    unsafe fn write_sized<T: Sized>(&mut self, value: &T) -> core::result::Result<(), Self::Error> {
        let buf = unsafe {
            core::slice::from_raw_parts(value as *const T as *const u8, core::mem::size_of::<T>())
        };
        self.write_all(buf)?;
        Ok(())
    }
}

pub trait Directory: ErrorType {
    type Entry: DirEntry;

    fn list(&self) -> Result<Vec<Self::Entry>, Self::Error>;
}

pub trait DirEntry {
    fn name(&self) -> &str;
    fn short_name(&self) -> &str {
        self.name()
    }
    fn is_directory(&self) -> bool;
    fn size(&self) -> usize;
}
