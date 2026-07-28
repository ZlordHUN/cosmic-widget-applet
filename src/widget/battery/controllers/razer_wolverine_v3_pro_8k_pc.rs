// SPDX-License-Identifier: MPL-2.0

//! Native Linux battery reader for the Razer Wolverine V3 Pro 8K PC.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const VENDOR_ID: u16 = 0x1532;
const WIRED_PRODUCT_ID: u16 = 0x0a57;
const DONGLE_PRODUCT_ID: u16 = 0x0a59;
const FEATURE_REPORT_ID: u8 = 0x0a;
const RAZER_REPORT_SIZE: usize = 90;
const HID_REPORT_SIZE: usize = RAZER_REPORT_SIZE + 1;
const TRANSACTION_ID: u8 = 0x1f;
const COMMAND_CLASS_POWER: u8 = 0x07;
const COMMAND_BATTERY_LEVEL: u8 = 0x80;
const COMMAND_CHARGING_STATUS: u8 = 0x84;
const COMMAND_SUCCESS: u8 = 0x02;
const RESPONSE_DELAY: Duration = Duration::from_millis(40);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BatteryState {
    pub(super) level: Option<u8>,
    pub(super) charging: bool,
    pub(super) connected: bool,
}

#[derive(Debug)]
struct HidrawDevice {
    path: PathBuf,
    product_id: u16,
}

impl HidrawDevice {
    fn is_wired(&self) -> bool {
        self.product_id == WIRED_PRODUCT_ID
    }
}

enum CommandResult {
    Success(u8),
    Unavailable,
}

pub(super) fn query() -> Result<Option<BatteryState>, String> {
    let mut devices = enumerate_hidraw().map_err(|error| error.to_string())?;
    devices.sort_by_key(|device| !device.is_wired());

    let Some(device) = devices.first() else {
        return Ok(None);
    };
    let mut handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&device.path)
        .map_err(|error| format!("failed to open {}: {error}", device.path.display()))?;

    let raw_level = match query_command(&mut handle, COMMAND_BATTERY_LEVEL)? {
        CommandResult::Success(level) => level,
        CommandResult::Unavailable => {
            return Ok(Some(BatteryState {
                level: None,
                charging: false,
                connected: false,
            }));
        }
    };
    let charging = match query_command(&mut handle, COMMAND_CHARGING_STATUS)? {
        CommandResult::Success(value) => value != 0,
        CommandResult::Unavailable => {
            return Err("Razer Wolverine V3 Pro 8K PC charging state was unavailable".to_string());
        }
    };

    Ok(Some(BatteryState {
        level: Some(scale_battery_level(raw_level)),
        charging,
        connected: true,
    }))
}

fn enumerate_hidraw() -> io::Result<Vec<HidrawDevice>> {
    let mut devices = Vec::new();
    for entry in fs::read_dir("/sys/class/hidraw")? {
        let entry = entry?;
        let uevent = match fs::read_to_string(entry.path().join("device/uevent")) {
            Ok(uevent) => uevent,
            Err(_) => continue,
        };
        let Some((vendor_id, product_id)) = parse_hid_id(&uevent) else {
            continue;
        };
        if vendor_id != VENDOR_ID || ![WIRED_PRODUCT_ID, DONGLE_PRODUCT_ID].contains(&product_id) {
            continue;
        }

        let descriptor = match fs::read(entry.path().join("device/report_descriptor")) {
            Ok(descriptor) => descriptor,
            Err(_) => continue,
        };
        if !supports_razer_feature_report(&descriptor) {
            continue;
        }

        devices.push(HidrawDevice {
            path: Path::new("/dev").join(entry.file_name()),
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
    Some((
        u16::try_from(vendor_id).ok()?,
        u16::try_from(product_id).ok()?,
    ))
}

fn supports_razer_feature_report(descriptor: &[u8]) -> bool {
    descriptor
        .windows(2)
        .any(|window| window == [0x85, FEATURE_REPORT_ID])
}

fn query_command(handle: &mut File, command: u8) -> Result<CommandResult, String> {
    let mut request = build_request(command);
    let written = unsafe {
        libc::ioctl(
            handle.as_raw_fd(),
            hid_ioc_feature(0x06, HID_REPORT_SIZE),
            request.as_mut_ptr(),
        )
    };
    if written != HID_REPORT_SIZE as libc::c_int {
        return Err(format!(
            "failed to send Razer Wolverine V3 Pro 8K PC feature report: {}",
            io::Error::last_os_error()
        ));
    }

    thread::sleep(RESPONSE_DELAY);

    let mut response = [0; HID_REPORT_SIZE];
    response[0] = FEATURE_REPORT_ID;
    let read = unsafe {
        libc::ioctl(
            handle.as_raw_fd(),
            hid_ioc_feature(0x07, HID_REPORT_SIZE),
            response.as_mut_ptr(),
        )
    };
    if read != HID_REPORT_SIZE as libc::c_int {
        return Err(format!(
            "failed to read Razer Wolverine V3 Pro 8K PC feature report: {}",
            io::Error::last_os_error()
        ));
    }

    parse_response(&response, command)
}

fn build_request(command: u8) -> [u8; HID_REPORT_SIZE] {
    let mut report = [0; HID_REPORT_SIZE];
    report[0] = FEATURE_REPORT_ID;
    report[2] = TRANSACTION_ID;
    report[6] = 0x02;
    report[7] = COMMAND_CLASS_POWER;
    report[8] = command;
    report[89] = calculate_crc(&report);
    report
}

fn parse_response(
    response: &[u8; HID_REPORT_SIZE],
    expected_command: u8,
) -> Result<CommandResult, String> {
    if response[0] != FEATURE_REPORT_ID
        || response[2] != TRANSACTION_ID
        || response[7] != COMMAND_CLASS_POWER
        || response[8] != expected_command
    {
        return Err(
            "Razer Wolverine V3 Pro 8K PC feature response did not match the request".to_string(),
        );
    }
    if calculate_crc(response) != response[89] {
        return Err(
            "Razer Wolverine V3 Pro 8K PC feature response checksum was invalid".to_string(),
        );
    }
    if response[1] != COMMAND_SUCCESS {
        return Ok(CommandResult::Unavailable);
    }

    Ok(CommandResult::Success(response[10]))
}

fn calculate_crc(report: &[u8; HID_REPORT_SIZE]) -> u8 {
    report[3..89]
        .iter()
        .fold(0, |checksum, byte| checksum ^ byte)
}

fn scale_battery_level(level: u8) -> u8 {
    ((u16::from(level) * 100 + 127) / 255) as u8
}

const fn hid_ioc_feature(number: libc::c_ulong, length: usize) -> libc::c_ulong {
    const IOC_WRITE: libc::c_ulong = 1;
    const IOC_READ: libc::c_ulong = 2;
    const IOC_SIZE_SHIFT: libc::c_ulong = 16;
    const IOC_DIR_SHIFT: libc::c_ulong = 30;

    ((IOC_READ | IOC_WRITE) << IOC_DIR_SHIFT)
        | ((length as libc::c_ulong) << IOC_SIZE_SHIFT)
        | ((b'H' as libc::c_ulong) << 8)
        | number
}

#[cfg(test)]
mod tests {
    use super::{
        COMMAND_BATTERY_LEVEL, COMMAND_CHARGING_STATUS, COMMAND_CLASS_POWER, COMMAND_SUCCESS,
        FEATURE_REPORT_ID, HID_REPORT_SIZE, TRANSACTION_ID, build_request, calculate_crc,
        hid_ioc_feature, parse_hid_id, parse_response, scale_battery_level,
        supports_razer_feature_report,
    };

    #[test]
    fn builds_razer_power_query_with_valid_checksum() {
        let report = build_request(COMMAND_BATTERY_LEVEL);

        assert_eq!(report[0], FEATURE_REPORT_ID);
        assert_eq!(report[2], TRANSACTION_ID);
        assert_eq!(report[6], 2);
        assert_eq!(report[7], COMMAND_CLASS_POWER);
        assert_eq!(report[8], COMMAND_BATTERY_LEVEL);
        assert_eq!(report[89], calculate_crc(&report));
    }

    #[test]
    fn parses_live_response_layout_and_scales_level() {
        let mut response = build_request(COMMAND_BATTERY_LEVEL);
        response[1] = COMMAND_SUCCESS;
        response[10] = 0xcc;
        response[89] = calculate_crc(&response);

        let value = match parse_response(&response, COMMAND_BATTERY_LEVEL).unwrap() {
            super::CommandResult::Success(value) => value,
            super::CommandResult::Unavailable => panic!("response should be available"),
        };

        assert_eq!(scale_battery_level(value), 80);
        assert_eq!(scale_battery_level(0xff), 100);
    }

    #[test]
    fn parses_charging_response() {
        let mut response = build_request(COMMAND_CHARGING_STATUS);
        response[1] = COMMAND_SUCCESS;
        response[10] = 1;
        response[89] = calculate_crc(&response);

        assert!(matches!(
            parse_response(&response, COMMAND_CHARGING_STATUS).unwrap(),
            super::CommandResult::Success(1)
        ));
    }

    #[test]
    fn accepts_only_wolverine_v3_pro_8k_pc_feature_interface() {
        assert!(supports_razer_feature_report(&[
            0x05,
            0x01,
            0x85,
            FEATURE_REPORT_ID,
            0x75,
            0x08,
        ]));
        assert!(!supports_razer_feature_report(&[
            0x06, 0x13, 0xff, 0x85, 0x06,
        ]));
    }

    #[test]
    fn parses_wolverine_v3_pro_8k_pc_hid_identity() {
        let uevent = "DRIVER=hid-generic\nHID_ID=0003:00001532:00000A59\n";
        assert_eq!(parse_hid_id(uevent), Some((0x1532, 0x0a59)));
    }

    #[test]
    fn matches_linux_feature_ioctl_codes() {
        assert_eq!(hid_ioc_feature(0x06, HID_REPORT_SIZE), 0xc05b_4806);
        assert_eq!(hid_ioc_feature(0x07, HID_REPORT_SIZE), 0xc05b_4807);
    }

    #[test]
    #[ignore = "requires a connected Razer Wolverine V3 Pro 8K PC"]
    fn reads_connected_wolverine_v3_pro_8k_pc() {
        let state = super::query()
            .expect("native Razer Wolverine V3 Pro 8K PC query failed")
            .expect("Razer Wolverine V3 Pro 8K PC dongle or wired controller was not found");
        if state.connected {
            assert!(state.level.is_some_and(|level| level <= 100));
        }
        println!("Razer Wolverine V3 Pro 8K PC native battery state: {state:?}");
    }
}
