#![allow(dead_code)]

extern crate alloc;

use alloc::vec::Vec;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};

use embedded_io_async::{Read, Write};
use esp_hal::{Async, usb_serial_jtag::{UsbSerialJtagRx, UsbSerialJtagTx}};
use log::info;

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

#[derive(Clone, Copy, Debug)]
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
}

impl UsbMode {
    pub fn new(max_payload: usize) -> Self {
        Self {
            state: UsbModeState::Idle,
            protocol: UsbProtocol::new(max_payload),
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

#[embassy_executor::task]
pub async fn usb_task(
    mut rx: UsbSerialJtagRx<'static, Async>,
    mut tx: UsbSerialJtagTx<'static, Async>,
    usb: &'static Mutex<CriticalSectionRawMutex, UsbMode>,
) -> ! {
    let mut buf = [0u8; 256];
    loop {
        match Read::read(&mut rx, &mut buf).await {
            Ok(len) => {
                if len == 0 {
                    continue;
                }
                let mut guard = usb.lock().await;
                guard.protocol().push_bytes(&buf[..len]);
                if guard.should_prompt() {
                    guard.enter_prompt();
                    info!("USB host activity detected: entering prompt");
                }
                loop {
                    let frame = match guard.protocol().next_frame() {
                        Some(Ok(frame)) => frame,
                        Some(Err(code)) => {
                            let response = encode_error(0, 0, code, "bad frame");
                            let _ = Write::write_all(&mut tx, &response).await;
                            continue;
                        }
                        None => break,
                    };
                    if guard.state() != UsbModeState::Active {
                        let response = encode_error(frame.req_id, frame.cmd, ErrorCode::Busy, "usb not active");
                        let _ = Write::write_all(&mut tx, &response).await;
                        continue;
                    }
                    let cmd = frame.cmd;
                    match cmd {
                        x if x == Command::Ping as u8 => {
                            let mut payload = Vec::new();
                            write_u32(&mut payload, 0x5854_3430); // "XT40"
                            let response = encode_ok(frame.req_id, cmd, &payload);
                            let _ = Write::write_all(&mut tx, &response).await;
                        }
                        x if x == Command::Info as u8 => {
                            let mut payload = Vec::new();
                            write_u32(&mut payload, guard.protocol().max_payload() as u32);
                            write_u32(&mut payload, 0); // capabilities (stub)
                            let response = encode_ok(frame.req_id, cmd, &payload);
                            let _ = Write::write_all(&mut tx, &response).await;
                        }
                        x if x == Command::Eject as u8 => {
                            guard.set_state(UsbModeState::Idle);
                            let response = encode_ok(frame.req_id, cmd, &[]);
                            let _ = Write::write_all(&mut tx, &response).await;
                        }
                        _ => {
                            let response = encode_error(
                                frame.req_id,
                                cmd,
                                ErrorCode::NotPermitted,
                                "not implemented",
                            );
                            let _ = Write::write_all(&mut tx, &response).await;
                        }
                    }
                }
            }
            Err(_) => {
                // TODO: handle disconnect
            }
        }
    }
}
