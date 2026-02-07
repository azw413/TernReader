# USB Serial File Protocol (X4)

This document specifies a simple, robust protocol for file management over the existing USB Serial/JTAG link on the X4 (ESP32-C3). It is designed to be easy to implement on both device and host and to keep the firmware in a safe “USB mode” while the host is connected.

## Goals
- Provide file management over USB Serial/JTAG (no MSC on ESP32-C3).
- Support listing, reading, writing, deleting, and creating directories.
- Be binary-safe and reliable on a lossy stream.
- Allow the device UI to enter a dedicated USB mode and block other functionality until “eject”.

## Transport
- Serial stream over USB Serial/JTAG.
- Default baud is set by the USB serial implementation; framing is done in-protocol.

## Framing
Each message is a binary frame:

```
MAGIC    u16  0x5452  ("TR")
VERSION  u8   0x01
FLAGS    u8   bitfield
CMD      u8
REQ_ID   u16  client-chosen, echoed in response
LEN      u32  payload length (bytes)
PAYLOAD  [LEN]
CRC32    u32  CRC of header+payload (MAGIC..PAYLOAD)
```

Notes:
- All integers are little-endian.
- `FLAGS`:
  - bit0: `RESP` (1 for responses)
  - bit1: `ERR` (1 if response indicates error)
  - bit2: `EOF` (1 if this is the last chunk)
  - bit3: `CONT` (1 if more chunks follow)
  - remaining bits reserved
- CRC32 uses the IEEE polynomial (standard `crc32`).

## Commands
All commands are request/response. Responses echo `REQ_ID` and `CMD`.

### `PING (0x01)`
Request payload: empty  
Response payload: `u32` protocol_id (`0x58543430` = "XT40")

### `INFO (0x02)`
Request payload: empty  
Response payload:
- `u32` max_payload (bytes per frame, e.g. 4096)
- `u32` capabilities bitfield
  - bit0: list
  - bit1: read
  - bit2: write
  - bit3: delete
  - bit4: mkdir
  - bit5: rmdir

### `LIST (0x10)`
Request payload:
- `u16` path_len
- `path_len` bytes: UTF-8 path (e.g. `/images`)

Response payload (chunked):
- `u16` entry_count
- Repeated entries:
  - `u8` kind (0=file, 1=dir)
  - `u16` name_len
  - `name_len` bytes: UTF-8 name
  - `u64` size_bytes (0 for dirs)

If `entry_count` is too large, device may split across multiple responses using `CONT` and `EOF`.

### `READ (0x11)`
Request payload:
- `u16` path_len
- `path_len` bytes: UTF-8 path
- `u64` offset
- `u32` length

Response payload:
- raw bytes, up to requested length

If the file is larger than `length`, host can issue additional READs.  
If the device limits payload size, it will use `CONT` and `EOF`.

### `WRITE (0x12)`
Request payload:
- `u16` path_len
- `path_len` bytes: UTF-8 path
- `u64` offset
- `u32` length
- `length` bytes: data

Response payload:
- `u32` written_bytes

Device should create the file if it does not exist.  
Host should send multiple chunks for large files.

### `DELETE (0x13)`
Request payload:
- `u16` path_len
- `path_len` bytes: UTF-8 path

Response payload: empty

### `MKDIR (0x14)`
Request payload:
- `u16` path_len
- `path_len` bytes: UTF-8 path

Response payload: empty

### `RMDIR (0x15)`
Request payload:
- `u16` path_len
- `path_len` bytes: UTF-8 path

Response payload: empty

### `RENAME (0x16)`
Request payload:
- `u16` from_len
- `from_len` bytes: UTF-8 path
- `u16` to_len
- `to_len` bytes: UTF-8 path

Response payload: empty

### `EJECT (0x20)`
Request payload: empty  
Response payload: empty  
Device exits USB mode, remounts SD for normal use.

## Errors
If a response has `ERR` flag set, payload is:
- `u16` code
- `u16` msg_len
- `msg_len` bytes: UTF-8 message

Suggested error codes:
- `1` invalid command
- `2` bad path
- `3` io error
- `4` not found
- `5` not permitted
- `6` crc mismatch
- `7` invalid args
- `8` busy

## USB Mode UI Flow
1. Detect USB host activity (or a `PING`).
2. Display a modal page:
   - Title: "USB Connected"
   - Message: "Enable USB file access?"
   - Options: `OK` / `Cancel`
3. If `OK`:
   - Enter USB mode (no other functions active).
   - Serve protocol commands.
4. If `Cancel`:
   - Ignore protocol commands until unplugged.
5. On `EJECT`:
   - Stop serving protocol.
   - Return to normal UI.

## Firmware Integration Notes
This section describes how to implement the protocol on the device.

### Detecting Host Presence
On ESP32-C3 with USB Serial/JTAG:
- Host presence can be inferred by incoming traffic.
- A minimal handshake is `PING` after the host connects.
- The firmware should only prompt on first activity (debounce repeated PINGs).

### USB Mode State
Add a `UsbMode` state machine in the firmware:
- `Idle`: default, normal UI.
- `Prompt`: show “Enable USB file access?”.
- `Active`: USB protocol active, normal UI suspended.
- `Rejected`: user declined, ignore protocol until unplug or timeout.

### UI Handling
- In `Prompt`, render a modal UI (blocking, no other app actions).
- If user confirms: transition to `Active`.
- If user cancels: transition to `Rejected` (ignore protocol; optionally show “USB disabled”).

### SD Card Exclusivity
When `Active`:
- Suspend application file access and background tasks that touch SD.
- Ensure SD access happens only through the USB protocol handlers.
- When leaving `Active`, reinitialize/mount SD if needed.

### Protocol Task
Implement a dedicated async task for serial RX/TX:
- Parse frames, validate CRC, dispatch commands.
- Use `REQ_ID` to correlate responses.
- For large responses, chunk with `CONT`/`EOF`.
- For errors, send `ERR` response with code/message.

### Disconnect / Eject
- If the host sends `EJECT`, respond OK and exit `Active`.
- If USB is unplugged (read errors or no activity for a timeout), exit `Active`.
- Remount SD and return to normal UI state.

### Minimum Viable Commands
Start with:
- `PING`, `INFO`, `LIST`, `READ`, `WRITE`, `DELETE`, `MKDIR`, `EJECT`

### Suggested Firmware Modules
Potential module layout:
- `x4/src/usb_mode.rs`:
  - `UsbModeState`
  - `UsbProtocol` (frame parsing + dispatch)
  - `UsbStorageBackend` (SD card adapter)
- `x4/src/main.rs`:
  - spawn USB task
  - integrate modal UI

### Blocking Behavior
While in `Active`:
- `Application::update` should be paused or short-circuited.
- Only the USB handler runs, plus a minimal UI loop for status.

## Concurrency & Safety
- When in USB mode, SD card access should be exclusive to the USB protocol.
- Application rendering and file access should be suspended.
- If host disconnects, exit USB mode and remount SD for the application.

## Host Tooling
A simple host CLI can:
- `list /images`
- `get /images/foo.tri`
- `put local.tri /images/foo.tri`
- `rm /images/foo.tri`
- `mkdir /images/new`
- `eject`

This protocol is intentionally minimal; it can be extended by adding new `CMD` values and bumping `VERSION` when breaking changes are introduced.
