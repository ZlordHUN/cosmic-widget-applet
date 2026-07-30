// SPDX-License-Identifier: MPL-2.0

//! Read-only support for Logitech's 64-byte Centurion HID++ transport.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use super::protocol::{BatteryFeature, BatteryReading};
use super::sysfs::CenturionReport;
use super::transport::next_software_id;

const FRAME_SIZE: usize = 64;
const FEATURE_SET: u16 = 0x0001;
const BRIDGE_FEATURE: u16 = 0x0003;
const BATTERY_FEATURE: u16 = 0x0104;
const REQUEST_TIMEOUT: Duration = Duration::from_millis(750);
const ADDRESS_PROBE_TIMEOUT: Duration = Duration::from_millis(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BatteryRoute {
    Direct { feature_index: u8 },
    Bridge { bridge_index: u8, feature_index: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Device {
    report: CenturionReport,
    address: Option<u8>,
    route: BatteryRoute,
}

pub(super) fn discover(handle: &mut File, report: CenturionReport) -> Result<Device, String> {
    let address = match report {
        CenturionReport::Standard => None,
        CenturionReport::Addressed => Some(probe_address(handle)?),
    };
    let mut device = Device {
        report,
        address,
        route: BatteryRoute::Direct { feature_index: 0 },
    };

    let features = enumerate_features(handle, &device)?;
    if let Some(feature_index) = feature_index(&features, BATTERY_FEATURE) {
        device.route = BatteryRoute::Direct { feature_index };
        return Ok(device);
    }

    let bridge_index = feature_index(&features, BRIDGE_FEATURE)
        .ok_or_else(|| "Centurion device exposes neither battery nor bridge feature".to_string())?;
    let sub_features = enumerate_bridge_features(handle, &device, bridge_index)?;
    let feature_index = feature_index(&sub_features, BATTERY_FEATURE)
        .ok_or_else(|| "Centurion headset exposes no battery feature".to_string())?;
    device.route = BatteryRoute::Bridge {
        bridge_index,
        feature_index,
    };
    Ok(device)
}

pub(super) fn query_battery(handle: &mut File, device: &Device) -> Result<BatteryReading, String> {
    let response = match device.route {
        BatteryRoute::Direct { feature_index } => {
            direct_request(handle, device, feature_index, 0x00, &[])?
        }
        BatteryRoute::Bridge {
            bridge_index,
            feature_index,
        } => bridge_request(handle, device, bridge_index, feature_index, 0x00, &[])?,
    };
    BatteryFeature::Centurion.parse(&response)
}

fn enumerate_features(handle: &mut File, device: &Device) -> Result<Vec<(u16, u8)>, String> {
    let feature_set = direct_request(handle, device, 0, 0x00, &FEATURE_SET.to_be_bytes())?
        .first()
        .copied()
        .filter(|index| *index != 0)
        .ok_or_else(|| "Centurion FeatureSet is unavailable".to_string())?;
    let count = direct_request(handle, device, feature_set, 0x00, &[])?
        .first()
        .copied()
        .ok_or_else(|| "Centurion feature count was missing".to_string())?;

    let mut features = vec![(FEATURE_SET, feature_set)];
    for index in 0..count {
        let Ok(response) = direct_request(handle, device, feature_set, 0x10, &[index]) else {
            continue;
        };
        if response.len() >= 3 {
            features.push((u16::from_be_bytes([response[1], response[2]]), index));
        }
    }
    Ok(features)
}

fn enumerate_bridge_features(
    handle: &mut File,
    device: &Device,
    bridge_index: u8,
) -> Result<Vec<(u16, u8)>, String> {
    let feature_set = bridge_request(
        handle,
        device,
        bridge_index,
        0,
        0x00,
        &FEATURE_SET.to_be_bytes(),
    )?
    .first()
    .copied()
    .filter(|index| *index != 0)
    .ok_or_else(|| "Centurion headset FeatureSet is unavailable".to_string())?;
    let count = bridge_request(handle, device, bridge_index, feature_set, 0x00, &[])?
        .first()
        .copied()
        .ok_or_else(|| "Centurion headset feature count was missing".to_string())?;

    let mut features = vec![(FEATURE_SET, feature_set)];
    let mut discovered_index = 0;
    for query_index in 0..count {
        let Ok(response) = bridge_request(
            handle,
            device,
            bridge_index,
            feature_set,
            0x10,
            &[query_index],
        ) else {
            continue;
        };
        if response.len() >= 3 {
            features.push((
                u16::from_be_bytes([response[1], response[2]]),
                discovered_index,
            ));
            discovered_index += 1;
        }
    }
    Ok(features)
}

fn feature_index(features: &[(u16, u8)], feature: u16) -> Option<u8> {
    features
        .iter()
        .find_map(|(id, index)| (*id == feature).then_some(*index))
}

fn direct_request(
    handle: &mut File,
    device: &Device,
    feature_index: u8,
    function: u8,
    params: &[u8],
) -> Result<Vec<u8>, String> {
    drain(handle);
    let function = (function & 0xf0) | next_software_id();
    let mut payload = Vec::with_capacity(params.len() + 2);
    payload.extend_from_slice(&[feature_index, function]);
    payload.extend_from_slice(params);
    write_frame(handle, device.report, device.address, 0, &payload)?;

    let started = Instant::now();
    while let Some(frame) = read_frame(handle, started, REQUEST_TIMEOUT)? {
        let response = unwrap_frame(&frame, device.report, device.address)?;
        if response.starts_with(&[0xff, feature_index, function]) {
            return Err(format!(
                "Centurion feature {feature_index:#04x} failed with error {:#04x}",
                response.get(3).copied().unwrap_or_default()
            ));
        }
        if response.starts_with(&[feature_index, function]) {
            return Ok(response[2..].to_vec());
        }
    }
    Err(format!(
        "Centurion feature {feature_index:#04x} request timed out"
    ))
}

fn bridge_request(
    handle: &mut File,
    device: &Device,
    bridge_index: u8,
    feature_index: u8,
    function: u8,
    params: &[u8],
) -> Result<Vec<u8>, String> {
    drain(handle);
    let software_id = next_software_id();
    let sub_function = (function & 0xf0) | software_id;
    let mut sub_message = Vec::with_capacity(params.len() + 3);
    sub_message.extend_from_slice(&[0, feature_index, sub_function]);
    sub_message.extend_from_slice(params);

    let sub_length =
        u16::try_from(sub_message.len()).map_err(|_| "Centurion bridge request is too large")?;
    let mut payload = Vec::with_capacity(sub_message.len() + 4);
    payload.extend_from_slice(&[
        bridge_index,
        0x10 | software_id,
        ((sub_length >> 8) & 0x0f) as u8,
        sub_length as u8,
    ]);
    payload.extend_from_slice(&sub_message);
    write_frame(handle, device.report, device.address, 0, &payload)?;

    let started = Instant::now();
    while let Some(frame) = read_frame(handle, started, REQUEST_TIMEOUT)? {
        let response = unwrap_frame(&frame, device.report, device.address)?;
        if response.len() < 7
            || response[0] != bridge_index
            || response[1] & 0xf0 != 0x10
            || response[1] & 0x0f != 0
            || response[4] != 0
        {
            continue;
        }
        if response[5] == 0xff {
            if response.get(6) == Some(&feature_index) {
                return Err(format!(
                    "Centurion bridged feature {feature_index:#04x} failed with error {:#04x}",
                    response.get(8).copied().unwrap_or_default()
                ));
            }
            continue;
        }
        if response[5] == feature_index && response[6] == sub_function {
            return Ok(response[7..].to_vec());
        }
    }
    Err(format!(
        "Centurion bridged feature {feature_index:#04x} request timed out"
    ))
}

fn probe_address(handle: &mut File) -> Result<u8, String> {
    drain(handle);
    let payload = [0x00, 0x10, 0x00, 0x00, 0x00];
    for address in 0..=u8::MAX {
        write_frame(
            handle,
            CenturionReport::Addressed,
            Some(address),
            0,
            &payload,
        )?;
        let started = Instant::now();
        if let Some(frame) = read_frame(handle, started, ADDRESS_PROBE_TIMEOUT)? {
            if frame[0] == CenturionReport::Addressed.id() {
                return Ok(frame[1]);
            }
        }
    }
    Err("Centurion device did not respond to address probing".to_string())
}

fn write_frame(
    handle: &mut File,
    report: CenturionReport,
    address: Option<u8>,
    flags: u8,
    payload: &[u8],
) -> Result<(), String> {
    let header_length = match report {
        CenturionReport::Standard => 3,
        CenturionReport::Addressed => 4,
    };
    if payload.len() > FRAME_SIZE - header_length {
        return Err("Centurion request exceeds one HID frame".to_string());
    }

    let mut frame = [0; FRAME_SIZE];
    frame[0] = report.id();
    let payload_start = match report {
        CenturionReport::Standard => {
            frame[1] =
                u8::try_from(payload.len() + 1).map_err(|_| "Centurion payload length overflow")?;
            frame[2] = flags;
            3
        }
        CenturionReport::Addressed => {
            frame[1] = address.ok_or_else(|| "Centurion device address is unknown".to_string())?;
            frame[2] =
                u8::try_from(payload.len() + 1).map_err(|_| "Centurion payload length overflow")?;
            frame[3] = flags;
            4
        }
    };
    frame[payload_start..payload_start + payload.len()].copy_from_slice(payload);
    handle
        .write_all(&frame)
        .map_err(|error| format!("failed to write Centurion request: {error}"))
}

fn unwrap_frame(
    frame: &[u8],
    report: CenturionReport,
    address: Option<u8>,
) -> Result<Vec<u8>, String> {
    if frame.first().copied() != Some(report.id()) {
        return Err("unexpected Centurion report ID".to_string());
    }
    let (length_index, flags_index, payload_start) = match report {
        CenturionReport::Standard => (1, 2, 3),
        CenturionReport::Addressed => {
            if address.is_some() && frame.get(1).copied() != address {
                return Err("unexpected Centurion device address".to_string());
            }
            (2, 3, 4)
        }
    };
    let length = usize::from(
        frame
            .get(length_index)
            .copied()
            .ok_or_else(|| "truncated Centurion frame".to_string())?,
    );
    if length == 0 || frame.get(flags_index).is_none() {
        return Err("invalid Centurion payload length".to_string());
    }
    let payload_length = length - 1;
    let payload_end = payload_start + payload_length;
    frame
        .get(payload_start..payload_end)
        .map(ToOwned::to_owned)
        .ok_or_else(|| "truncated Centurion payload".to_string())
}

fn read_frame(
    handle: &mut File,
    started: Instant,
    timeout: Duration,
) -> Result<Option<[u8; FRAME_SIZE]>, String> {
    let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
        return Ok(None);
    };
    let timeout_ms = i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
    let mut pollfd = libc::pollfd {
        fd: handle.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return read_frame(handle, started, timeout);
        }
        return Err(format!("failed to poll Centurion endpoint: {error}"));
    }
    if ready == 0 {
        return Ok(None);
    }

    let mut frame = [0; FRAME_SIZE];
    match handle.read(&mut frame) {
        Ok(FRAME_SIZE) => Ok(Some(frame)),
        Ok(read) => Err(format!("short Centurion frame: {read} bytes")),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            read_frame(handle, started, timeout)
        }
        Err(error) => Err(format!("failed to read Centurion response: {error}")),
    }
}

fn drain(handle: &mut File) {
    let mut stale = [0; FRAME_SIZE];
    while handle.read(&mut stale).is_ok_and(|read| read > 0) {}
}

#[cfg(test)]
mod tests {
    use super::{CenturionReport, unwrap_frame};

    #[test]
    fn unwraps_standard_frames() {
        let mut frame = [0; 64];
        frame[..8].copy_from_slice(&[0x51, 6, 0, 4, 0x0e, 80, 80, 1]);
        assert_eq!(
            unwrap_frame(&frame, CenturionReport::Standard, None),
            Ok(vec![4, 0x0e, 80, 80, 1])
        );
    }

    #[test]
    fn unwraps_addressed_frames() {
        let mut frame = [0; 64];
        frame[..9].copy_from_slice(&[0x50, 0x23, 6, 0, 4, 0x0e, 80, 80, 3]);
        assert_eq!(
            unwrap_frame(&frame, CenturionReport::Addressed, Some(0x23)),
            Ok(vec![4, 0x0e, 80, 80, 3])
        );
    }

    #[test]
    fn rejects_another_address() {
        let mut frame = [0; 64];
        frame[..7].copy_from_slice(&[0x50, 0x24, 4, 0, 4, 0x0e, 80]);
        assert!(unwrap_frame(&frame, CenturionReport::Addressed, Some(0x23)).is_err());
    }
}
