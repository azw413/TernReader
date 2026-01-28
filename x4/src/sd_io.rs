extern crate alloc;

use core::cmp::min;

use embedded_sdmmc::{Block, BlockDevice, BlockIdx};
use core_io::{Error, ErrorKind, Read, Seek, SeekFrom, Write};

pub struct SdCardIo<'a, D>
where
    D: BlockDevice,
    D::Error: core::fmt::Debug,
{
    sdcard: &'a D,
    pos: u64,
    base_lba: u32,
    total_blocks: u32,
}

impl<'a, D> SdCardIo<'a, D>
where
    D: BlockDevice,
    D::Error: core::fmt::Debug,
{
    pub fn new(sdcard: &'a D, base_lba: u32) -> Result<Self, Error> {
        let total_blocks = sdcard
            .num_blocks()
            .map_err(|_| Error::new(ErrorKind::Other, "sdmmc"))?
            .0;
        Ok(Self {
            sdcard,
            pos: 0,
            base_lba,
            total_blocks,
        })
    }

    fn read_block(&self, lba: u32, block: &mut Block) -> Result<(), Error> {
        self.sdcard
            .read(core::slice::from_mut(block), BlockIdx(lba))
            .map_err(|_| Error::new(ErrorKind::Other, "sdmmc"))
    }

    fn write_block(&self, lba: u32, block: &Block) -> Result<(), Error> {
        self.sdcard
            .write(core::slice::from_ref(block), BlockIdx(lba))
            .map_err(|_| Error::new(ErrorKind::Other, "sdmmc"))
    }

    fn max_bytes(&self) -> u64 {
        let available = self.total_blocks.saturating_sub(self.base_lba) as u64;
        available * Block::LEN as u64
    }
}

impl<D> Read for SdCardIo<'_, D>
where
    D: BlockDevice,
    D::Error: core::fmt::Debug,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.pos >= self.max_bytes() {
            return Ok(0);
        }

        let mut offset = 0;
        let mut remaining = buf.len();
        while remaining > 0 && self.pos < self.max_bytes() {
            let lba = (self.pos / Block::LEN as u64) as u32;
            let block_offset = (self.pos % Block::LEN as u64) as usize;
            let mut block = Block::new();
            self.read_block(self.base_lba + lba, &mut block)?;

            let take = min(remaining, Block::LEN - block_offset);
            buf[offset..offset + take]
                .copy_from_slice(&block.contents[block_offset..block_offset + take]);

            offset += take;
            remaining -= take;
            self.pos += take as u64;
        }

        Ok(offset)
    }
}

impl<D> Write for SdCardIo<'_, D>
where
    D: BlockDevice,
    D::Error: core::fmt::Debug,
{
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut offset = 0;
        let mut remaining = buf.len();
        while remaining > 0 {
            let lba = (self.pos / Block::LEN as u64) as u32;
            let block_offset = (self.pos % Block::LEN as u64) as usize;
            let write_len = min(remaining, Block::LEN - block_offset);

            if block_offset == 0 && write_len == Block::LEN {
                let mut block = Block::new();
                block.contents.copy_from_slice(&buf[offset..offset + Block::LEN]);
                self.write_block(self.base_lba + lba, &block)?;
            } else {
                let mut block = Block::new();
                self.read_block(self.base_lba + lba, &mut block)?;
                block.contents[block_offset..block_offset + write_len]
                    .copy_from_slice(&buf[offset..offset + write_len]);
                self.write_block(self.base_lba + lba, &block)?;
            }

            offset += write_len;
            remaining -= write_len;
            self.pos += write_len as u64;
        }

        Ok(offset)
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl<D> Seek for SdCardIo<'_, D>
where
    D: BlockDevice,
    D::Error: core::fmt::Debug,
{
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Error> {
        let max = self.max_bytes();
        let next = match pos {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(offset) => {
                if offset >= 0 {
                    self.pos.saturating_add(offset as u64)
                } else {
                    self.pos
                        .checked_sub((-offset) as u64)
                        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "seek"))?
                }
            }
            SeekFrom::End(offset) => {
                if offset >= 0 {
                    max.saturating_add(offset as u64)
                } else {
                    max.checked_sub((-offset) as u64)
                        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "seek"))?
                }
            }
        };
        self.pos = next;
        Ok(self.pos)
    }
}

pub fn detect_fat_partition<D>(sdcard: &D) -> Result<u32, Error>
where
    D: BlockDevice,
    D::Error: core::fmt::Debug,
{
    let mut block = Block::new();
    sdcard
        .read(core::slice::from_mut(&mut block), BlockIdx(0))
        .map_err(|_| Error::new(ErrorKind::Other, "sdmmc"))?;

    let signature = &block.contents[510..512];
    if signature != [0x55, 0xAA] {
        return Ok(0);
    }

    let fat_types = [0x01u8, 0x04, 0x06, 0x0B, 0x0C, 0x0E];
    for idx in 0..4 {
        let start = 446 + (idx * 16);
        let entry = &block.contents[start..start + 16];
        let part_type = entry[4];
        let lba = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);
        if lba != 0 && fat_types.contains(&part_type) {
            return Ok(lba);
        }
    }

    Ok(0)
}
