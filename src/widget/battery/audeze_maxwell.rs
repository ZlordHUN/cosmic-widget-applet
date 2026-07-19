// SPDX-License-Identifier: MPL-2.0

//! Native Audeze Maxwell battery reader for Linux hidraw.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const VENDOR_ID: u16 = 0x3329;
const DONGLE_PRODUCT_IDS: [u16; 2] = [0x4b19, 0x4b18];
const WIRED_PRODUCT_IDS: [u16; 2] = [0x4b1a, 0x4b1e];
const MESSAGE_SIZE: usize = 62;
const INPUT_REPORT_ID: u8 = 0x07;
const PACKET_DELAY: Duration = Duration::from_millis(60);

const STATUS_REQUESTS: [[u8; MESSAGE_SIZE]; 5] = [
    packet(&[0x06, 0x08, 0x80, 0x05, 0x5a, 0x04, 0x00, 0x01, 0x09, 0x22]),
    packet(&[0x06, 0x08, 0x80, 0x05, 0x5a, 0x04, 0x00, 0x01, 0x09]),
    packet(&[0x06, 0x08, 0x80, 0x05, 0x5a, 0x04, 0x00, 0x83, 0x2c, 0x0b]),
    packet(&[0x06, 0x08, 0x80, 0x05, 0x5a, 0x04, 0x00, 0x01, 0x09, 0x2c]),
    packet(&[0x06, 0x08, 0x80, 0x05, 0x5a, 0x04, 0x00, 0x83, 0x2c, 0x07]),
];
const BATTERY_REQUEST: [u8; MESSAGE_SIZE] =
    packet(&[0x06, 0x07, 0x80, 0x05, 0x5a, 0x03, 0x00, 0xd6, 0x0c]);

const fn packet(bytes: &[u8]) -> [u8; MESSAGE_SIZE] {
    let mut packet = [0; MESSAGE_SIZE];
    let mut index = 0;
    while index < bytes.len() {
        packet[index] = bytes[index];
        index += 1;
    }
    packet
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BatteryState {
    pub(super) level: Option<u8>,
    pub(super) charging: bool,
}

#[derive(Debug)]
struct HidrawDevice {
    path: PathBuf,
    vendor_id: u16,
    product_id: u16,
}

pub(super) fn query() -> Result<Option<BatteryState>, String> {
    let devices = enumerate_hidraw().map_err(|error| error.to_string())?;
    let Some(dongle) = devices.iter().find(|device| {
        device.vendor_id == VENDOR_ID && DONGLE_PRODUCT_IDS.contains(&device.product_id)
    }) else {
        return Ok(None);
    };
    let charging = devices.iter().any(|device| {
        device.vendor_id == VENDOR_ID && WIRED_PRODUCT_IDS.contains(&device.product_id)
    });

    let mut handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&dongle.path)
        .map_err(|error| format!("failed to open {}: {error}", dongle.path.display()))?;
    let mut status_responses = Vec::with_capacity(STATUS_REQUESTS.len());
    for request in &STATUS_REQUESTS {
        status_responses.push(send_request(&mut handle, request)?);
    }
    let battery_response = send_request(&mut handle, &BATTERY_REQUEST)?;
    let level = parse_battery_response(&battery_response).or_else(|| {
        status_responses
            .iter()
            .find_map(|response| parse_battery_response(response))
    });

    Ok(Some(BatteryState { level, charging }))
}

fn enumerate_hidraw() -> io::Result<Vec<HidrawDevice>> {
    let mut devices = Vec::new();
    for entry in fs::read_dir("/sys/class/hidraw")? {
        let entry = entry?;
        let uevent_path = entry.path().join("device/uevent");
        let Ok(uevent) = fs::read_to_string(uevent_path) else {
            continue;
        };
        let Some((vendor_id, product_id)) = parse_hid_id(&uevent) else {
            continue;
        };

        devices.push(HidrawDevice {
            path: Path::new("/dev").join(entry.file_name()),
            vendor_id,
            product_id,
        });
    }
    Ok(devices)
}

fn parse_hid_id(uevent: &str) -> Option<(u16, u16)> {
    let value = uevent
        .lines()
        .find_map(|line| line.strip_prefix("HID_ID="))?;
    let mut parts = value.split(':');
    parts.next()?;
    let vendor_id = u32::from_str_radix(parts.next()?, 16).ok()?;
    let product_id = u32::from_str_radix(parts.next()?, 16).ok()?;
    Some((u16::try_from(vendor_id).ok()?, u16::try_from(product_id).ok()?))
}

fn send_request(
    handle: &mut File,
    request: &[u8; MESSAGE_SIZE],
) -> Result<[u8; MESSAGE_SIZE], String> {
    thread::sleep(PACKET_DELAY);
    handle
        .write_all(request)
        .map_err(|error| format!("failed to write Maxwell HID report: {error}"))?;

    let mut response = [0; MESSAGE_SIZE];
    response[0] = INPUT_REPORT_ID;
    let read = unsafe {
        libc::ioctl(
            handle.as_raw_fd(),
            hid_ioc_get_input(MESSAGE_SIZE),
            response.as_mut_ptr(),
        )
    };
    if read != MESSAGE_SIZE as libc::c_int {
        return Err(format!(
            "failed to read Maxwell HID input report: {}",
            io::Error::last_os_error()
        ));
    }

    Ok(response)
}

fn parse_battery_response(response: &[u8]) -> Option<u8> {
    response.windows(5).find_map(|window| {
        (window[0..4] == [0xd6, 0x0c, 0x00, 0x00] && window[4] <= 100)
            .then_some(window[4])
    })
}

const fn hid_ioc_get_input(length: usize) -> libc::c_ulong {
    const IOC_WRITE: libc::c_ulong = 1;
    const IOC_READ: libc::c_ulong = 2;
    const IOC_SIZE_SHIFT: libc::c_ulong = 16;
    const IOC_DIR_SHIFT: libc::c_ulong = 30;

    ((IOC_READ | IOC_WRITE) << IOC_DIR_SHIFT)
        | ((length as libc::c_ulong) << IOC_SIZE_SHIFT)
        | ((b'H' as libc::c_ulong) << 8)
        | 0x0a
}

#[cfg(test)]
mod tests {
    use super::{MESSAGE_SIZE, hid_ioc_get_input, parse_battery_response, parse_hid_id, query};

    #[test]
    fn parses_maxwell_hid_identity() {
        let uevent = "DRIVER=hid-generic\nHID_ID=0003:00003329:00004B18\n";
        assert_eq!(parse_hid_id(uevent), Some((0x3329, 0x4b18)));
    }

    #[test]
    fn parses_battery_marker_and_rejects_invalid_levels() {
        assert_eq!(
            parse_battery_response(&[0x07, 0xd6, 0x0c, 0x00, 0x00, 98]),
            Some(98)
        );
        assert_eq!(
            parse_battery_response(&[0x07, 0xd6, 0x0c, 0x00, 0x00, 101]),
            None
        );
    }

    #[test]
    fn matches_linux_hidiocginput_request_code() {
        assert_eq!(hid_ioc_get_input(MESSAGE_SIZE), 0xc03e_480a);
    }

    #[test]
    #[ignore = "requires a connected Audeze Maxwell"]
    fn reads_connected_maxwell() {
        let state = query().expect("native Maxwell query failed");
        let state = state.expect("Maxwell dongle was not found");
        assert!(state.level.is_some_and(|level| level <= 100));
        println!("Maxwell native battery state: {state:?}");
    }
}
