// SPDX-License-Identifier: MPL-2.0

//! Raw Linux hidraw transport for Logitech HID++ requests.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

const HIDPP_SHORT_REPORT_ID: u8 = 0x10;
const HIDPP_LONG_REPORT_ID: u8 = 0x11;
const REQUEST_TIMEOUT: Duration = Duration::from_millis(250);
static NEXT_SOFTWARE_ID: AtomicU8 = AtomicU8::new(1);

pub(super) fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

pub(super) fn hidpp20_request(
    handle: &mut File,
    slot: u8,
    request_id: u16,
    params: &[u8],
) -> Result<Vec<u8>, String> {
    send_request(
        handle,
        HIDPP_LONG_REPORT_ID,
        slot,
        with_software_id(request_id, next_software_id()),
        params,
    )
}

pub(super) fn next_software_id() -> u8 {
    NEXT_SOFTWARE_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(if current >= 0x0f { 1 } else { current + 1 })
        })
        .unwrap_or(1)
}

fn with_software_id(request_id: u16, software_id: u8) -> u16 {
    (request_id & 0xfff0) | u16::from(software_id & 0x0f)
}

pub(super) fn receiver_request(
    handle: &mut File,
    request_id: u16,
    params: &[u8],
) -> Result<Vec<u8>, String> {
    send_request(handle, HIDPP_SHORT_REPORT_ID, 0xff, request_id, params)
}

#[cfg(test)]
mod tests {
    use super::with_software_id;

    #[test]
    fn replaces_only_the_hidpp_software_id() {
        assert_eq!(with_software_id(0x071e, 5), 0x0715);
        assert_eq!(with_software_id(0x000e, 0x0f), 0x000f);
    }
}

pub(super) fn hidpp10_register(
    handle: &mut File,
    slot: u8,
    register: u16,
) -> Result<Vec<u8>, String> {
    send_request(handle, HIDPP_SHORT_REPORT_ID, slot, 0x8100 | register, &[])
}

fn send_request(
    handle: &mut File,
    report_id: u8,
    slot: u8,
    request_id: u16,
    params: &[u8],
) -> Result<Vec<u8>, String> {
    if params.len() > 16 {
        return Err("HID++ request has too many parameters".to_string());
    }

    let mut stale = [0; 32];
    while handle.read(&mut stale).is_ok_and(|read| read > 0) {}

    let mut packet = [0; 20];
    packet[0] = report_id;
    packet[1] = slot;
    packet[2..4].copy_from_slice(&request_id.to_be_bytes());
    packet[4..4 + params.len()].copy_from_slice(params);
    let packet_length = if report_id == HIDPP_SHORT_REPORT_ID {
        7
    } else {
        packet.len()
    };
    handle
        .write_all(&packet[..packet_length])
        .map_err(|error| format!("failed to write HID++ request: {error}"))?;

    let started = Instant::now();
    loop {
        let Some(remaining) = REQUEST_TIMEOUT.checked_sub(started.elapsed()) else {
            return Err(format!("HID++ request {request_id:#06x} timed out"));
        };
        let timeout = i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
        let mut pollfd = libc::pollfd {
            fd: handle.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pollfd, 1, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("failed to poll HID++ endpoint: {error}"));
        }
        if ready == 0 {
            return Err(format!("HID++ request {request_id:#06x} timed out"));
        }

        let mut response = [0; 64];
        let read = match handle.read(&mut response) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(format!("failed to read HID++ response: {error}")),
        };
        if read < 5 || (response[1] != slot && response[1] != slot ^ 0xff) {
            continue;
        }
        if response[2] == 0xff && response[3] == packet[2] && response[4] == packet[3] {
            return Err(format!(
                "HID++ request {request_id:#06x} failed with error {:#04x}",
                response.get(5).copied().unwrap_or_default()
            ));
        }
        if response[2] == 0x8f && response[3] == packet[2] && response[4] == packet[3] {
            return Err(format!(
                "HID++ receiver request {request_id:#06x} failed with error {:#04x}",
                response.get(5).copied().unwrap_or_default()
            ));
        }
        if response[2..4] == packet[2..4] {
            if slot == 0xff && request_id == 0x83b5 && response.get(4) != params.first() {
                continue;
            }
            return Ok(response[4..read].to_vec());
        }
    }
}
