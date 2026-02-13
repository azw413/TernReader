#![allow(dead_code)]

extern crate alloc;

use alloc::{string::{String, ToString}, vec::Vec};
use embedded_io_async::{Read, Write};
use esp_hal::{Async, usb_serial_jtag::{UsbSerialJtagRx, UsbSerialJtagTx}};
use embassy_time::{Duration, with_timeout};
use crate::image_source::{UsbStorage, UsbDirEntry};
use tern_core::image_viewer::ImageError;

const MAGIC: u16 = 0x5452; // "TR"
const VERSION: u8 = 0x01;

const FLAG_RESP: u8 = 1 << 0;
const FLAG_ERR: u8 = 1 << 1;
const FLAG_EOF: u8 = 1 << 2;
const FLAG_CONT: u8 = 1 << 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbModeState {
    Idle,
    Prompt,
    Active,
    Rejected,
}

#[derive(Clone, Copy, Debug)]
pub enum Command {
    Ping = 0x01,
    Info = 0x02,
    List = 0x10,
    Read = 0x11,
    Write = 0x12,
    Delete = 0x13,
    Mkdir = 0x14,
    Rmdir = 0x15,
    Rename = 0x16,
    Eject = 0x20,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidCommand = 1,
    BadPath = 2,
    Io = 3,
    NotFound = 4,
    NotPermitted = 5,
    CrcMismatch = 6,
    InvalidArgs = 7,
    Busy = 8,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub flags: u8,
    pub cmd: u8,
    pub req_id: u16,
    pub payload: Vec<u8>,
}

pub struct UsbProtocol {
    rx_buf: Vec<u8>,
    max_payload: usize,
}

impl UsbProtocol {
    pub fn new(max_payload: usize) -> Self {
        Self {
            rx_buf: Vec::new(),
            max_payload,
        }
    }

    pub fn max_payload(&self) -> usize {
        self.max_payload
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.rx_buf.extend_from_slice(bytes);
    }

    pub fn next_frame(&mut self) -> Option<Result<Frame, ErrorCode>> {
        if self.rx_buf.len() < 2 + 1 + 1 + 1 + 2 + 4 + 4 {
            return None;
        }
        let magic = u16::from_le_bytes([self.rx_buf[0], self.rx_buf[1]]);
        if magic != MAGIC {
            self.rx_buf.remove(0);
            return Some(Err(ErrorCode::InvalidArgs));
        }
        let version = self.rx_buf[2];
        if version != VERSION {
            self.rx_buf.remove(0);
            return Some(Err(ErrorCode::InvalidArgs));
        }
        let flags = self.rx_buf[3];
        let cmd = self.rx_buf[4];
        let req_id = u16::from_le_bytes([self.rx_buf[5], self.rx_buf[6]]);
        let len = u32::from_le_bytes([
            self.rx_buf[7],
            self.rx_buf[8],
            self.rx_buf[9],
            self.rx_buf[10],
        ]) as usize;
        let total = 2 + 1 + 1 + 1 + 2 + 4 + len + 4;
        if self.rx_buf.len() < total {
            return None;
        }
        let payload_start = 11;
        let payload_end = payload_start + len;
        let crc_start = payload_end;
        let expected_crc = u32::from_le_bytes([
            self.rx_buf[crc_start],
            self.rx_buf[crc_start + 1],
            self.rx_buf[crc_start + 2],
            self.rx_buf[crc_start + 3],
        ]);
        let actual_crc = crc32(&self.rx_buf[0..payload_end]);
        if expected_crc != actual_crc {
            self.rx_buf.drain(0..total);
            return Some(Err(ErrorCode::CrcMismatch));
        }
        let payload = self.rx_buf[payload_start..payload_end].to_vec();
        self.rx_buf.drain(0..total);
        Some(Ok(Frame {
            flags,
            cmd,
            req_id,
            payload,
        }))
    }
}

pub struct UsbMode {
    state: UsbModeState,
    protocol: UsbProtocol,
    last_cmd: Option<u8>,
    last_req: Option<u16>,
    last_err: Option<ErrorCode>,
    last_list_count: Option<u16>,
    write_session: Option<WriteSession>,
}

impl UsbMode {
    pub fn new(max_payload: usize) -> Self {
        Self {
            state: UsbModeState::Idle,
            protocol: UsbProtocol::new(max_payload),
            last_cmd: None,
            last_req: None,
            last_err: None,
            last_list_count: None,
            write_session: None,
        }
    }

    pub fn state(&self) -> UsbModeState {
        self.state
    }

    pub fn set_state(&mut self, state: UsbModeState) {
        self.state = state;
    }

    pub fn protocol(&mut self) -> &mut UsbProtocol {
        &mut self.protocol
    }

    pub fn status(&self) -> UsbStatus {
        UsbStatus {
            last_cmd: self.last_cmd,
            last_req: self.last_req,
            last_err: self.last_err,
            last_list_count: self.last_list_count,
        }
    }

    pub fn should_prompt(&self) -> bool {
        matches!(self.state, UsbModeState::Idle)
    }

    pub fn enter_prompt(&mut self) {
        self.state = UsbModeState::Prompt;
    }

    pub fn accept(&mut self) {
        self.state = UsbModeState::Active;
    }

    pub fn reject(&mut self) {
        self.state = UsbModeState::Rejected;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbStatus {
    pub last_cmd: Option<u8>,
    pub last_req: Option<u16>,
    pub last_err: Option<ErrorCode>,
    pub last_list_count: Option<u16>,
}

#[derive(Clone, Debug)]
struct WriteSession {
    req_id: u16,
    path: String,
    offset: u64,
    total_len: u64,
    written: u64,
}

fn write_u16(buf: &mut Vec<u8>, value: u16) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn encode_frame(flags: u8, cmd: u8, req_id: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 1 + 1 + 1 + 2 + 4 + payload.len() + 4);
    write_u16(&mut out, MAGIC);
    out.push(VERSION);
    out.push(flags);
    out.push(cmd);
    write_u16(&mut out, req_id);
    write_u32(&mut out, payload.len() as u32);
    out.extend_from_slice(payload);
    let crc = crc32(&out);
    write_u32(&mut out, crc);
    out
}

fn encode_error(req_id: u16, cmd: u8, code: ErrorCode, message: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    write_u16(&mut payload, code as u16);
    write_u16(&mut payload, message.len() as u16);
    payload.extend_from_slice(message.as_bytes());
    encode_frame(FLAG_RESP | FLAG_ERR, cmd, req_id, &payload)
}

fn encode_error_for(req_id: u16, cmd: u8, code: ErrorCode, err: ImageError, fallback: &str) -> Vec<u8> {
    match err {
        ImageError::Message(msg) => encode_error(req_id, cmd, code, &msg),
        _ => encode_error(req_id, cmd, code, fallback),
    }
}

fn encode_ok(req_id: u16, cmd: u8, payload: &[u8]) -> Vec<u8> {
    encode_frame(FLAG_RESP, cmd, req_id, payload)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB88320 & mask);
        }
    }
    !crc
}

fn read_u16(data: &[u8], cursor: &mut usize) -> Option<u16> {
    if *cursor + 2 > data.len() {
        return None;
    }
    let value = u16::from_le_bytes([data[*cursor], data[*cursor + 1]]);
    *cursor += 2;
    Some(value)
}

fn read_u32(data: &[u8], cursor: &mut usize) -> Option<u32> {
    if *cursor + 4 > data.len() {
        return None;
    }
    let value = u32::from_le_bytes([
        data[*cursor],
        data[*cursor + 1],
        data[*cursor + 2],
        data[*cursor + 3],
    ]);
    *cursor += 4;
    Some(value)
}

fn read_u64(data: &[u8], cursor: &mut usize) -> Option<u64> {
    if *cursor + 8 > data.len() {
        return None;
    }
    let value = u64::from_le_bytes([
        data[*cursor],
        data[*cursor + 1],
        data[*cursor + 2],
        data[*cursor + 3],
        data[*cursor + 4],
        data[*cursor + 5],
        data[*cursor + 6],
        data[*cursor + 7],
    ]);
    *cursor += 8;
    Some(value)
}

fn read_path(data: &[u8], cursor: &mut usize) -> Option<String> {
    let len = read_u16(data, cursor)? as usize;
    if *cursor + len > data.len() {
        return None;
    }
    let path = core::str::from_utf8(&data[*cursor..*cursor + len]).ok()?;
    *cursor += len;
    Some(path.to_string())
}



fn serialize_list(entries: &[UsbDirEntry]) -> Vec<u8> {
    let mut payload = Vec::new();
    write_u16(&mut payload, entries.len() as u16);
    for entry in entries {
        payload.push(if entry.is_dir { 1 } else { 0 });
        write_u16(&mut payload, entry.name.len() as u16);
        payload.extend_from_slice(entry.name.as_bytes());
        payload.extend_from_slice(&entry.size.to_le_bytes());
    }
    payload
}

fn send_chunked<'a>(
    tx: &'a mut UsbSerialJtagTx<'static, Async>,
    cmd: u8,
    req_id: u16,
    payload: &'a [u8],
    max_payload: usize,
) -> impl core::future::Future<Output = ()> + 'a {
    async move {
        if payload.len() <= max_payload {
            let response = encode_ok(req_id, cmd, payload);
            let _ = Write::write_all(tx, &response).await;
            return;
        }
        let mut offset = 0usize;
        while offset < payload.len() {
            let end = (offset + max_payload).min(payload.len());
            let mut flags = FLAG_RESP | FLAG_CONT;
            if end >= payload.len() {
                flags = FLAG_RESP | FLAG_EOF;
            }
            let chunk = encode_frame(flags, cmd, req_id, &payload[offset..end]);
            let _ = Write::write_all(tx, &chunk).await;
            offset = end;
        }
    }
}

pub async fn poll<S: UsbStorage>(
    usb: &mut UsbMode,
    rx: &mut UsbSerialJtagRx<'static, Async>,
    tx: &mut UsbSerialJtagTx<'static, Async>,
    storage: &mut S,
) {
    let mut buf = [0u8; 2048];
    let read = with_timeout(Duration::from_millis(20), Read::read(rx, &mut buf)).await;
    if let Ok(Ok(len)) = read {
        if len > 0 {
            usb.protocol.push_bytes(&buf[..len]);
            if usb.should_prompt() {
                usb.accept();
            }
        }
    }

    loop {
        let frame = match usb.protocol.next_frame() {
            Some(Ok(frame)) => frame,
            Some(Err(code)) => {
                usb.last_err = Some(code);
                let response = encode_error(0, 0, code, "bad frame");
                let _ = Write::write_all(tx, &response).await;
                continue;
            }
            None => break,
        };
        usb.last_cmd = Some(frame.cmd);
        usb.last_req = Some(frame.req_id);
        if usb.state() != UsbModeState::Active {
            usb.last_err = Some(ErrorCode::Busy);
            let response = encode_error(frame.req_id, frame.cmd, ErrorCode::Busy, "usb not active");
            let _ = Write::write_all(tx, &response).await;
            continue;
        }
        let cmd = frame.cmd;
        match cmd {
            x if x == Command::Ping as u8 => {
                let mut payload = Vec::new();
                write_u32(&mut payload, 0x5854_3430); // "XT40"
                usb.last_err = None;
                let response = encode_ok(frame.req_id, cmd, &payload);
                let _ = Write::write_all(tx, &response).await;
            }
            x if x == Command::Info as u8 => {
                let mut payload = Vec::new();
                write_u32(&mut payload, usb.protocol.max_payload() as u32);
                write_u32(&mut payload, 0x0000_003F); // list/read/write/delete/mkdir/rmdir
                usb.last_err = None;
                let response = encode_ok(frame.req_id, cmd, &payload);
                let _ = Write::write_all(tx, &response).await;
            }
            x if x == Command::List as u8 => {
                let mut cursor = 0usize;
                let Some(path) = read_path(&frame.payload, &mut cursor) else {
                    usb.last_err = Some(ErrorCode::InvalidArgs);
                    let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad path");
                    let _ = Write::write_all(tx, &response).await;
                    continue;
                };
                match storage.usb_list(&path) {
                    Ok(entries) => {
                        usb.last_err = None;
                        usb.last_list_count = Some(entries.len() as u16);
                        let payload = serialize_list(&entries);
                        send_chunked(tx, cmd, frame.req_id, &payload, usb.protocol.max_payload()).await;
                    }
                    Err(err) => {
                        usb.last_err = Some(ErrorCode::Io);
                        let response = encode_error_for(frame.req_id, cmd, ErrorCode::Io, err, "list failed");
                        let _ = Write::write_all(tx, &response).await;
                    }
                }
            }
            x if x == Command::Read as u8 => {
                let mut cursor = 0usize;
                let Some(path) = read_path(&frame.payload, &mut cursor) else {
                    usb.last_err = Some(ErrorCode::InvalidArgs);
                    let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad path");
                    let _ = Write::write_all(tx, &response).await;
                    continue;
                };
                let Some(offset) = read_u64(&frame.payload, &mut cursor) else {
                    usb.last_err = Some(ErrorCode::InvalidArgs);
                    let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad offset");
                    let _ = Write::write_all(tx, &response).await;
                    continue;
                };
                let Some(length) = read_u32(&frame.payload, &mut cursor) else {
                    usb.last_err = Some(ErrorCode::InvalidArgs);
                    let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad length");
                    let _ = Write::write_all(tx, &response).await;
                    continue;
                };
                match storage.usb_read(&path, offset, length) {
                    Ok(data) => {
                        usb.last_err = None;
                        send_chunked(tx, cmd, frame.req_id, &data, usb.protocol.max_payload()).await;
                    }
                    Err(err) => {
                        usb.last_err = Some(ErrorCode::Io);
                        let response = encode_error_for(frame.req_id, cmd, ErrorCode::Io, err, "read failed");
                        let _ = Write::write_all(tx, &response).await;
                    }
                }
            }
            x if x == Command::Write as u8 => {
                let mut cursor = 0usize;
                let is_stream = (frame.flags & (FLAG_CONT | FLAG_EOF)) != 0;
                if is_stream {
                    let header_len = if frame.payload.len() >= 2 {
                        u16::from_le_bytes([frame.payload[0], frame.payload[1]]) as usize
                    } else {
                        0
                    };
                    let header_needed = 2 + header_len + 4 + 8;
                    let header_utf8_ok = header_len > 0
                        && frame.payload.len() >= header_needed
                        && core::str::from_utf8(&frame.payload[2..2 + header_len]).is_ok();
                    let has_header = if usb.write_session.is_none() {
                        header_utf8_ok
                    } else if header_utf8_ok {
                        if let Some(session) = usb.write_session.as_ref() {
                            let path_bytes = session.path.as_bytes();
                            header_len == path_bytes.len()
                                && frame.payload[2..2 + header_len] == path_bytes[..]
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if usb.write_session.is_none() {
                        if !has_header {
                            usb.last_err = Some(ErrorCode::InvalidArgs);
                            let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "missing header");
                            let _ = Write::write_all(tx, &response).await;
                            continue;
                        }
                        let Some(path) = read_path(&frame.payload, &mut cursor) else {
                            usb.last_err = Some(ErrorCode::InvalidArgs);
                            let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad path");
                            let _ = Write::write_all(tx, &response).await;
                            continue;
                        };
                        let Some(total_len) = read_u32(&frame.payload, &mut cursor) else {
                            usb.last_err = Some(ErrorCode::InvalidArgs);
                            let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad total");
                            let _ = Write::write_all(tx, &response).await;
                            continue;
                        };
                        usb.write_session = Some(WriteSession {
                            req_id: frame.req_id,
                            path,
                            offset: 0,
                            total_len: total_len as u64,
                            written: 0,
                        });
                    } else if has_header {
                        let Some(path) = read_path(&frame.payload, &mut cursor) else {
                            usb.last_err = Some(ErrorCode::InvalidArgs);
                            let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad path");
                            let _ = Write::write_all(tx, &response).await;
                            continue;
                        };
                        let Some(total_len) = read_u32(&frame.payload, &mut cursor) else {
                            usb.last_err = Some(ErrorCode::InvalidArgs);
                            let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad total");
                            let _ = Write::write_all(tx, &response).await;
                            continue;
                        };
                        if let Some(session) = usb.write_session.as_ref() {
                            if !session.path.eq_ignore_ascii_case(&path) {
                                usb.last_err = Some(ErrorCode::InvalidArgs);
                                let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "path mismatch");
                                let _ = Write::write_all(tx, &response).await;
                                continue;
                            }
                            if session.total_len != total_len as u64 {
                                usb.last_err = Some(ErrorCode::InvalidArgs);
                                let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "total mismatch");
                                let _ = Write::write_all(tx, &response).await;
                                continue;
                            }
                        }
                    }
                    let Some(session) = usb.write_session.as_mut() else {
                        continue;
                    };
                    if session.req_id != frame.req_id {
                        usb.last_err = Some(ErrorCode::Busy);
                        let response = encode_error(frame.req_id, cmd, ErrorCode::Busy, "write busy");
                        let _ = Write::write_all(tx, &response).await;
                        continue;
                    }
                    let Some(offset) = read_u64(&frame.payload, &mut cursor) else {
                        usb.last_err = Some(ErrorCode::InvalidArgs);
                        let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad offset");
                        let _ = Write::write_all(tx, &response).await;
                        continue;
                    };
                    if offset > session.written {
                        usb.last_err = Some(ErrorCode::InvalidArgs);
                        let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "offset ahead");
                        let _ = Write::write_all(tx, &response).await;
                        continue;
                    }
                    if offset < session.written {
                        let mut payload = Vec::new();
                        write_u32(&mut payload, session.written as u32);
                        let response = encode_frame(FLAG_RESP | FLAG_CONT, cmd, frame.req_id, &payload);
                        let _ = Write::write_all(tx, &response).await;
                        continue;
                    }
                    let data = &frame.payload[cursor..];
                    let write_offset = session.offset + session.written;
                    let final_chunk = (frame.flags & FLAG_EOF) != 0;
                    match storage.usb_write_stream(&session.path, write_offset, data, final_chunk) {
                        Ok(written) => {
                            session.written = session.written.saturating_add(written as u64);
                            let mut payload = Vec::new();
                            write_u32(&mut payload, session.written as u32);
                            let mut resp_flags = FLAG_RESP;
                            if final_chunk {
                                if session.written != session.total_len {
                                    usb.last_err = Some(ErrorCode::Io);
                                    let response = encode_error(
                                        frame.req_id,
                                        cmd,
                                        ErrorCode::Io,
                                        "write length mismatch",
                                    );
                                    let _ = Write::write_all(tx, &response).await;
                                    usb.write_session = None;
                                    continue;
                                }
                                resp_flags |= FLAG_EOF;
                                usb.last_err = None;
                                let response = encode_frame(resp_flags, cmd, frame.req_id, &payload);
                                let _ = Write::write_all(tx, &response).await;
                                usb.write_session = None;
                            } else {
                                resp_flags |= FLAG_CONT;
                                let response = encode_frame(resp_flags, cmd, frame.req_id, &payload);
                                let _ = Write::write_all(tx, &response).await;
                            }
                        }
                        Err(err) => {
                            usb.last_err = Some(ErrorCode::Io);
                            let response = encode_error_for(frame.req_id, cmd, ErrorCode::Io, err, "write failed");
                            let _ = Write::write_all(tx, &response).await;
                            usb.write_session = None;
                        }
                    }
                } else {
                    let Some(path) = read_path(&frame.payload, &mut cursor) else {
                        usb.last_err = Some(ErrorCode::InvalidArgs);
                        let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad path");
                        let _ = Write::write_all(tx, &response).await;
                        continue;
                    };
                    let Some(offset) = read_u64(&frame.payload, &mut cursor) else {
                        usb.last_err = Some(ErrorCode::InvalidArgs);
                        let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad offset");
                        let _ = Write::write_all(tx, &response).await;
                        continue;
                    };
                    let Some(length) = read_u32(&frame.payload, &mut cursor) else {
                        usb.last_err = Some(ErrorCode::InvalidArgs);
                        let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad length");
                        let _ = Write::write_all(tx, &response).await;
                        continue;
                    };
                    if cursor + (length as usize) > frame.payload.len() {
                        usb.last_err = Some(ErrorCode::InvalidArgs);
                        let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad data");
                        let _ = Write::write_all(tx, &response).await;
                        continue;
                    }
                    let data = &frame.payload[cursor..cursor + length as usize];
                    match storage.usb_write(&path, offset, data) {
                        Ok(written) => {
                            usb.last_err = None;
                            let mut payload = Vec::new();
                            write_u32(&mut payload, written);
                            let response = encode_ok(frame.req_id, cmd, &payload);
                            let _ = Write::write_all(tx, &response).await;
                        }
                        Err(err) => {
                            usb.last_err = Some(ErrorCode::Io);
                            let response = encode_error_for(frame.req_id, cmd, ErrorCode::Io, err, "write failed");
                            let _ = Write::write_all(tx, &response).await;
                        }
                    }
                }
            }
            x if x == Command::Delete as u8 => {
                let mut cursor = 0usize;
                let Some(path) = read_path(&frame.payload, &mut cursor) else {
                    usb.last_err = Some(ErrorCode::InvalidArgs);
                    let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad path");
                    let _ = Write::write_all(tx, &response).await;
                    continue;
                };
                match storage.usb_delete(&path) {
                    Ok(()) => {
                        usb.last_err = None;
                        let response = encode_ok(frame.req_id, cmd, &[]);
                        let _ = Write::write_all(tx, &response).await;
                    }
                    Err(err) => {
                        usb.last_err = Some(ErrorCode::Io);
                        let response = encode_error_for(frame.req_id, cmd, ErrorCode::Io, err, "delete failed");
                        let _ = Write::write_all(tx, &response).await;
                    }
                }
            }
            x if x == Command::Mkdir as u8 => {
                let mut cursor = 0usize;
                let Some(path) = read_path(&frame.payload, &mut cursor) else {
                    usb.last_err = Some(ErrorCode::InvalidArgs);
                    let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad path");
                    let _ = Write::write_all(tx, &response).await;
                    continue;
                };
                match storage.usb_mkdir(&path) {
                    Ok(()) => {
                        usb.last_err = None;
                        let response = encode_ok(frame.req_id, cmd, &[]);
                        let _ = Write::write_all(tx, &response).await;
                    }
                    Err(err) => {
                        usb.last_err = Some(ErrorCode::Io);
                        let response = encode_error_for(frame.req_id, cmd, ErrorCode::Io, err, "mkdir failed");
                        let _ = Write::write_all(tx, &response).await;
                    }
                }
            }
            x if x == Command::Rmdir as u8 => {
                usb.last_err = Some(ErrorCode::NotPermitted);
                let response = encode_error(frame.req_id, cmd, ErrorCode::NotPermitted, "rmdir not supported");
                let _ = Write::write_all(tx, &response).await;
            }
            x if x == Command::Rename as u8 => {
                let mut cursor = 0usize;
                let Some(from) = read_path(&frame.payload, &mut cursor) else {
                    usb.last_err = Some(ErrorCode::InvalidArgs);
                    let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad from");
                    let _ = Write::write_all(tx, &response).await;
                    continue;
                };
                let Some(to) = read_path(&frame.payload, &mut cursor) else {
                    usb.last_err = Some(ErrorCode::InvalidArgs);
                    let response = encode_error(frame.req_id, cmd, ErrorCode::InvalidArgs, "bad to");
                    let _ = Write::write_all(tx, &response).await;
                    continue;
                };
                match storage.usb_rename(&from, &to) {
                    Ok(()) => {
                        usb.last_err = None;
                        let response = encode_ok(frame.req_id, cmd, &[]);
                        let _ = Write::write_all(tx, &response).await;
                    }
                    Err(err) => {
                        usb.last_err = Some(ErrorCode::Io);
                        let response = encode_error_for(frame.req_id, cmd, ErrorCode::Io, err, "rename failed");
                        let _ = Write::write_all(tx, &response).await;
                    }
                }
            }
            x if x == Command::Eject as u8 => {
                usb.last_err = None;
                usb.set_state(UsbModeState::Idle);
                let response = encode_ok(frame.req_id, cmd, &[]);
                let _ = Write::write_all(tx, &response).await;
            }
            _ => {
                usb.last_err = Some(ErrorCode::InvalidCommand);
                let response = encode_error(
                    frame.req_id,
                    cmd,
                    ErrorCode::InvalidCommand,
                    "unknown command",
                );
                let _ = Write::write_all(tx, &response).await;
            }
        }
    }
}
