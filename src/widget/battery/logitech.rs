// SPDX-License-Identifier: MPL-2.0

//! Native Logitech battery readers for Linux power supplies and Bolt receivers.

use std::fs::{self, File, OpenOptions};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const POWER_SUPPLY_ROOT: &str = "/sys/class/power_supply";
const HIDRAW_ROOT: &str = "/sys/class/hidraw";
const LOGITECH_VENDOR_ID: u16 = 0x046d;
const BOLT_RECEIVER_PRODUCT_ID: u16 = 0xc548;
const HIDPP_SHORT_REPORT_ID: u8 = 0x10;
const HIDPP_LONG_REPORT_ID: u8 = 0x11;
const HIDPP_SOFTWARE_ID: u16 = 0x0e;
const DEVICE_NAME_FEATURE: u16 = 0x0005;
const UNIFIED_BATTERY_FEATURE: u16 = 0x1004;
const REQUEST_TIMEOUT: Duration = Duration::from_millis(250);
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BatteryState {
    pub(super) name: String,
    pub(super) level: Option<u8>,
    pub(super) status: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HidppDevice {
    slot: u8,
    name: String,
    kind: Option<String>,
    battery_feature: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PairedDevice {
    slot: u8,
    name: Option<String>,
    kind: Option<String>,
}

pub(super) struct Monitor {
    bolt_path: Option<PathBuf>,
    bolt_devices: Vec<HidppDevice>,
    last_bolt_readings: HashMap<u8, (Option<u8>, Option<String>)>,
    last_discovery: Option<Instant>,
}

impl Monitor {
    pub(super) fn new() -> Self {
        Self {
            bolt_path: None,
            bolt_devices: Vec::new(),
            last_bolt_readings: HashMap::new(),
            last_discovery: None,
        }
    }

    pub(super) fn query(&mut self) -> Vec<BatteryState> {
        let mut states = query_power_supplies_at(Path::new(POWER_SUPPLY_ROOT));

        let Some(path) = find_bolt_receiver_at(Path::new(HIDRAW_ROOT)) else {
            self.bolt_path = None;
            self.bolt_devices.clear();
            self.last_bolt_readings.clear();
            self.last_discovery = None;
            return states;
        };

        if self.bolt_path.as_ref() != Some(&path) {
            self.bolt_path = Some(path.clone());
            self.bolt_devices.clear();
            self.last_bolt_readings.clear();
            self.last_discovery = None;
        }

        let Ok(mut handle) = open_hidraw(&path) else {
            return states;
        };

        let discovery_due = self
            .last_discovery
            .is_none_or(|last| last.elapsed() >= DISCOVERY_INTERVAL);
        if discovery_due {
            let discovered = match paired_bolt_devices(&mut handle) {
                Ok(paired_devices) => {
                    drop(handle);
                    let Ok(fresh_handle) = open_hidraw(&path) else {
                        return states;
                    };
                    handle = fresh_handle;
                    let mut discovered = Vec::new();
                    // Sleeping Bolt endpoints can ignore the first feature call.
                    // Reopening routes each retry through a fresh hidraw queue.
                    for attempt in 0..3 {
                        if attempt > 0 {
                            let Ok(fresh_handle) = open_hidraw(&path) else {
                                break;
                            };
                            handle = fresh_handle;
                            thread::sleep(Duration::from_millis(75));
                        }
                        discovered = discover_paired_devices(
                            &mut handle,
                            paired_devices.clone(),
                        );
                        if !discovered.is_empty() {
                            break;
                        }
                    }
                    discovered
                }
                Err(_) => discover_devices_by_slot(&mut handle),
            };
            if !discovered.is_empty() || self.bolt_devices.is_empty() {
                self.bolt_devices = discovered;
            }
            self.last_discovery = (!self.bolt_devices.is_empty()).then(Instant::now);
        }

        for device in &self.bolt_devices {
            let reading = query_unified_battery(&mut handle, device);
            let state = state_from_bolt_reading(
                device,
                reading,
                &mut self.last_bolt_readings,
            );
            upsert_state(&mut states, state);
        }

        states
    }
}

fn state_from_bolt_reading(
    device: &HidppDevice,
    reading: Result<(Option<u8>, Option<String>), String>,
    last_readings: &mut HashMap<u8, (Option<u8>, Option<String>)>,
) -> BatteryState {
    let (level, status, connected) = match reading {
        Ok((level, status)) => {
            last_readings.insert(device.slot, (level, status.clone()));
            (level, status, true)
        }
        Err(_) => match last_readings.get(&device.slot) {
            Some((level, status)) => (*level, status.clone(), true),
            None => (None, None, false),
        },
    };

    BatteryState {
        name: device.name.clone(),
        level,
        status,
        kind: device.kind.clone(),
        connected,
    }
}

fn query_power_supplies_at(root: &Path) -> Vec<BatteryState> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };

    entries
        .filter_map(Result::ok)
        .filter_map(|entry| parse_power_supply(&entry.path()))
        .collect()
}

fn parse_power_supply(path: &Path) -> Option<BatteryState> {
    let manufacturer = read_trimmed(path.join("manufacturer"))?;
    let scope = read_trimmed(path.join("scope"))?;
    if !manufacturer.eq_ignore_ascii_case("Logitech") || !scope.eq_ignore_ascii_case("Device") {
        return None;
    }

    let name = read_trimmed(path.join("model_name"))?;
    let connected = read_trimmed(path.join("online"))
        .map(|online| online != "0")
        .unwrap_or(true);
    let level = connected
        .then(|| read_trimmed(path.join("capacity")))
        .flatten()
        .and_then(|capacity| capacity.parse::<u8>().ok())
        .filter(|capacity| *capacity <= 100);
    let status = connected
        .then(|| read_trimmed(path.join("status")))
        .flatten()
        .and_then(normalize_power_status);

    Some(BatteryState {
        kind: infer_kind(&name),
        name,
        level,
        status,
        connected,
    })
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_power_status(status: String) -> Option<String> {
    match status.to_ascii_lowercase().as_str() {
        "charging" => Some("charging".to_string()),
        "full" => Some("charged".to_string()),
        "discharging" | "not charging" => Some("discharging".to_string()),
        "unknown" => None,
        other => Some(other.to_string()),
    }
}

fn infer_kind(name: &str) -> Option<String> {
    let name = name.to_ascii_lowercase();
    let kind = if name.contains("keyboard")
        || name.contains("mechanical")
        || name.contains("keys")
    {
        "keyboard"
    } else if name.contains("mouse")
        || name.starts_with('g') && name[1..].starts_with(|character: char| character.is_ascii_digit())
        || name.contains("master")
        || name.contains("anywhere")
    {
        "mouse"
    } else {
        return None;
    };
    Some(kind.to_string())
}

fn find_bolt_receiver_at(root: &Path) -> Option<PathBuf> {
    fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let uevent = fs::read_to_string(entry.path().join("device/uevent")).ok()?;
            let (vendor_id, product_id) = parse_hid_id(&uevent)?;
            let physical_path = uevent
                .lines()
                .find_map(|line| line.strip_prefix("HID_PHYS="))?;
            (vendor_id == LOGITECH_VENDOR_ID
                && product_id == BOLT_RECEIVER_PRODUCT_ID
                && physical_path.ends_with("/input2"))
            .then(|| Path::new("/dev").join(entry.file_name()))
        })
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

fn open_hidraw(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

fn discover_paired_devices(
    handle: &mut File,
    paired_devices: Vec<PairedDevice>,
) -> Vec<HidppDevice> {
    paired_devices
        .into_iter()
        .filter_map(|paired| discover_device(handle, paired).ok())
        .collect()
}

fn discover_devices_by_slot(handle: &mut File) -> Vec<HidppDevice> {
    (1..=6)
        .filter_map(|slot| {
            discover_device(
                handle,
                PairedDevice {
                    slot,
                    name: None,
                    kind: None,
                },
            )
            .ok()
        })
        .collect()
}

fn paired_bolt_devices(handle: &mut File) -> Result<Vec<PairedDevice>, String> {
    let connection = receiver_request(handle, 0x8102, &[])?;
    let expected = connection
        .get(1)
        .copied()
        .ok_or_else(|| "Bolt receiver connection count was missing".to_string())? as usize;
    let mut devices = Vec::with_capacity(expected);

    for slot in 1..=6 {
        if devices.len() >= expected {
            break;
        }
        let Ok(pairing) = receiver_request(handle, 0x83b5, &[0x50 + slot]) else {
            continue;
        };
        let Some(kind) = pairing.get(1).copied().map(|value| value & 0x0f) else {
            continue;
        };
        let product_id = parse_bolt_product_id(&pairing);
        devices.push(PairedDevice {
            slot,
            name: product_id.and_then(known_logitech_name).map(str::to_string),
            kind: hidpp10_kind(kind),
        });
    }

    if expected > 0 && devices.is_empty() {
        Err("Bolt receiver did not return its paired devices".to_string())
    } else {
        Ok(devices)
    }
}

fn parse_bolt_product_id(response: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes([*response.get(3)?, *response.get(2)?]))
}

fn known_logitech_name(product_id: u16) -> Option<&'static str> {
    match product_id {
        0xb367 => Some("MX Mechanical Mini"),
        _ => None,
    }
}

fn hidpp10_kind(kind: u8) -> Option<String> {
    let kind = match kind {
        0x01 => "keyboard",
        0x02 => "mouse",
        0x03 => "numpad",
        0x04 => "presenter",
        0x08 => "trackball",
        0x09 => "touchpad",
        0x0d => "headset",
        _ => return None,
    };
    Some(kind.to_string())
}

fn discover_device(handle: &mut File, paired: PairedDevice) -> Result<HidppDevice, String> {
    let slot = paired.slot;
    let battery_feature = feature_index(handle, slot, UNIFIED_BATTERY_FEATURE)?;
    let (name, kind) = if let Some(name) = paired.name {
        let kind = paired.kind.or_else(|| infer_kind(&name));
        (name, kind)
    } else {
        let name_feature = feature_index(handle, slot, DEVICE_NAME_FEATURE).ok();
        let name = name_feature
            .and_then(|feature| query_device_name(handle, slot, feature).ok())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("Logitech device {slot}"));
        let kind = name_feature
            .and_then(|feature| query_device_kind(handle, slot, feature).ok())
            .or(paired.kind)
            .or_else(|| infer_kind(&name));
        (name, kind)
    };

    Ok(HidppDevice {
        slot,
        name,
        kind,
        battery_feature,
    })
}

fn feature_index(handle: &mut File, slot: u8, feature: u16) -> Result<u8, String> {
    let response = hidpp_request(
        handle,
        slot,
        HIDPP_SOFTWARE_ID,
        &feature.to_be_bytes(),
    )?;
    response
        .first()
        .copied()
        .filter(|index| *index != 0)
        .ok_or_else(|| format!("HID++ feature {feature:#06x} is unavailable"))
}

fn query_device_name(handle: &mut File, slot: u8, feature: u8) -> Result<String, String> {
    let length = hidpp_request(
        handle,
        slot,
        (u16::from(feature) << 8) | HIDPP_SOFTWARE_ID,
        &[],
    )?
    .first()
    .copied()
    .ok_or_else(|| "HID++ device name length was missing".to_string())? as usize;
    let mut name = Vec::with_capacity(length);

    while name.len() < length {
        let offset = u8::try_from(name.len())
            .map_err(|_| "HID++ device name is too long".to_string())?;
        let fragment = hidpp_request(
            handle,
            slot,
            (u16::from(feature) << 8) | 0x10 | HIDPP_SOFTWARE_ID,
            &[offset],
        )?;
        if fragment.is_empty() {
            return Err("HID++ device name fragment was empty".to_string());
        }
        name.extend_from_slice(&fragment[..fragment.len().min(length - name.len())]);
    }

    String::from_utf8(name).map_err(|error| format!("invalid HID++ device name: {error}"))
}

fn query_device_kind(handle: &mut File, slot: u8, feature: u8) -> Result<String, String> {
    let response = hidpp_request(
        handle,
        slot,
        (u16::from(feature) << 8) | 0x20 | HIDPP_SOFTWARE_ID,
        &[],
    )?;
    match response.first().copied() {
        Some(0x00) => Ok("keyboard".to_string()),
        Some(0x02) => Ok("numpad".to_string()),
        Some(0x03) => Ok("mouse".to_string()),
        Some(0x04) => Ok("touchpad".to_string()),
        Some(0x05) => Ok("trackball".to_string()),
        Some(0x06) => Ok("presenter".to_string()),
        _ => Err("unknown HID++ device kind".to_string()),
    }
}

fn query_unified_battery(
    handle: &mut File,
    device: &HidppDevice,
) -> Result<(Option<u8>, Option<String>), String> {
    let response = hidpp_request(
        handle,
        device.slot,
        (u16::from(device.battery_feature) << 8) | 0x10 | HIDPP_SOFTWARE_ID,
        &[],
    )?;
    parse_unified_battery(&response)
}

fn parse_unified_battery(response: &[u8]) -> Result<(Option<u8>, Option<String>), String> {
    let [discharge, approximation, status, ..] = response else {
        return Err("HID++ unified battery response was too short".to_string());
    };
    let level = if *discharge > 0 && *discharge <= 100 {
        Some(*discharge)
    } else {
        match approximation {
            8 => Some(90),
            4 => Some(50),
            2 => Some(20),
            1 => Some(5),
            _ => None,
        }
    };
    let status = match status {
        0x00 => Some("discharging".to_string()),
        0x01 | 0x02 | 0x04 => Some("charging".to_string()),
        0x03 => Some("charged".to_string()),
        0x05 => Some("invalid battery".to_string()),
        0x06 => Some("thermal error".to_string()),
        _ => None,
    };
    Ok((level, status))
}

fn hidpp_request(
    handle: &mut File,
    slot: u8,
    request_id: u16,
    params: &[u8],
) -> Result<Vec<u8>, String> {
    send_hidpp_request(handle, HIDPP_LONG_REPORT_ID, slot, request_id, params)
}

fn receiver_request(
    handle: &mut File,
    request_id: u16,
    params: &[u8],
) -> Result<Vec<u8>, String> {
    send_hidpp_request(handle, HIDPP_SHORT_REPORT_ID, 0xff, request_id, params)
}

fn send_hidpp_request(
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
            return Err(format!("failed to poll HID++ receiver: {}", io::Error::last_os_error()));
        }
        if ready == 0 {
            return Err(format!("HID++ request {request_id:#06x} timed out"));
        }

        let mut response = [0; 32];
        let read = match handle.read(&mut response) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(format!("failed to read HID++ response: {error}")),
        };
        if read < 5 || response[1] != slot {
            continue;
        }
        if response[2] == 0xff
            && response[3] == packet[2]
            && response[4] == packet[3]
        {
            return Err(format!(
                "HID++ request {request_id:#06x} failed with error {:#04x}",
                response.get(5).copied().unwrap_or_default()
            ));
        }
        if response[2] == 0x8f
            && response[3] == packet[2]
            && response[4] == packet[3]
        {
            return Err(format!(
                "HID++ receiver request {request_id:#06x} failed with error {:#04x}",
                response.get(5).copied().unwrap_or_default()
            ));
        }
        if response[2..4] == packet[2..4] {
            if slot == 0xff
                && request_id == 0x83b5
                && response.get(4) != params.first()
            {
                continue;
            }
            return Ok(response[4..read].to_vec());
        }
    }
}

fn upsert_state(states: &mut Vec<BatteryState>, state: BatteryState) {
    if let Some(existing) = states
        .iter_mut()
        .find(|existing| existing.name.eq_ignore_ascii_case(&state.name))
    {
        *existing = state;
    } else {
        states.push(state);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HidppDevice, infer_kind, parse_bolt_product_id, parse_hid_id,
        parse_unified_battery, state_from_bolt_reading,
    };
    use std::collections::HashMap;

    #[test]
    fn parses_logitech_hid_identity() {
        let uevent = concat!(
            "HID_ID=0003:0000046D:0000C548\n",
            "HID_NAME=Logitech USB Receiver\n",
            "HID_PHYS=usb-test/input2\n",
        );
        assert_eq!(parse_hid_id(uevent), Some((0x046d, 0xc548)));
    }

    #[test]
    fn parses_unified_battery_percentage_and_status() {
        assert_eq!(
            parse_unified_battery(&[20, 2, 0, 0]),
            Ok((Some(20), Some("discharging".to_string())))
        );
        assert_eq!(
            parse_unified_battery(&[0, 4, 1, 0]),
            Ok((Some(50), Some("charging".to_string())))
        );
    }

    #[test]
    fn infers_current_logitech_device_kinds() {
        assert_eq!(infer_kind("G309 LIGHTSPEED").as_deref(), Some("mouse"));
        assert_eq!(
            infer_kind("MX Mechanical Mini").as_deref(),
            Some("keyboard")
        );
    }

    #[test]
    fn parses_bolt_receiver_product_id() {
        assert_eq!(
            parse_bolt_product_id(&[
                0x54, 0x01, 0x67, 0xb3, 0x79, 0x66, 0x51, 0xb5,
            ]),
            Some(0xb367)
        );
    }

    #[test]
    fn preserves_last_bolt_reading_while_device_sleeps() {
        let device = HidppDevice {
            slot: 4,
            name: "MX Mechanical Mini".to_string(),
            kind: Some("keyboard".to_string()),
            battery_feature: 7,
        };
        let mut readings = HashMap::new();

        let awake = state_from_bolt_reading(
            &device,
            Ok((Some(20), Some("discharging".to_string()))),
            &mut readings,
        );
        let sleeping = state_from_bolt_reading(
            &device,
            Err("device timed out".to_string()),
            &mut readings,
        );

        assert_eq!(sleeping.level, awake.level);
        assert_eq!(sleeping.status, awake.status);
        assert!(sleeping.connected);
    }

    #[test]
    #[ignore = "requires connected Logitech devices"]
    fn reads_connected_logitech_devices() {
        let started = std::time::Instant::now();
        let mut monitor = super::Monitor::new();
        let states = monitor.query();
        assert!(!states.is_empty());
        assert!(states.iter().any(|state| state.name == "G309 LIGHTSPEED"));
        assert!(states.iter().any(|state| state.name == "MX Mechanical Mini"));
        for state in &states {
            assert!(state.level.is_none_or(|level| level <= 100));
        }
        println!(
            "Logitech native battery states in {:?}: {states:?}",
            started.elapsed()
        );
    }
}
