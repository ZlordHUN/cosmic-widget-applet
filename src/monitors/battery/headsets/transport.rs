// SPDX-License-Identifier: MPL-2.0

//! Shared HID helpers for native headset battery protocols.

use hidapi::HidDevice;

pub(super) const QUERY_TIMEOUT_MS: i32 = 400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Reading {
    pub(super) level: Option<u8>,
    pub(super) status: Option<String>,
}

impl Reading {
    pub(super) fn discharging(level: u8) -> Self {
        Self {
            level: Some(level.min(100)),
            status: Some("discharging".to_string()),
        }
    }

    pub(super) fn charging(level: Option<u8>) -> Self {
        Self {
            level: level.map(|level| level.min(100)),
            status: Some("charging".to_string()),
        }
    }
}

pub(super) fn write(device: &HidDevice, data: &[u8]) -> Result<(), String> {
    device
        .write(data)
        .map(|_| ())
        .map_err(|error| format!("HID write failed: {error}"))
}

pub(super) fn write_padded(device: &HidDevice, prefix: &[u8], length: usize) -> Result<(), String> {
    if prefix.len() > length {
        return Err("HID request prefix exceeds its report length".to_string());
    }
    let mut report = vec![0; length];
    report[..prefix.len()].copy_from_slice(prefix);
    write(device, &report)
}

pub(super) fn read(device: &HidDevice, length: usize, timeout_ms: i32) -> Result<Vec<u8>, String> {
    let mut response = vec![0; length];
    let read = device
        .read_timeout(&mut response, timeout_ms)
        .map_err(|error| format!("HID read failed: {error}"))?;
    if read == 0 {
        return Err("HID read timed out".to_string());
    }
    response.truncate(read);
    Ok(response)
}

pub(super) fn send_feature(device: &HidDevice, prefix: &[u8], length: usize) -> Result<(), String> {
    if prefix.len() > length {
        return Err("HID feature prefix exceeds its report length".to_string());
    }
    let mut report = vec![0; length];
    report[..prefix.len()].copy_from_slice(prefix);
    device
        .send_feature_report(&report)
        .map(|_| ())
        .map_err(|error| format!("HID feature write failed: {error}"))
}

pub(super) fn get_feature(
    device: &HidDevice,
    report_id: u8,
    length: usize,
) -> Result<Vec<u8>, String> {
    let mut response = vec![0; length];
    response[0] = report_id;
    let read = device
        .get_feature_report(&mut response)
        .map_err(|error| format!("HID feature read failed: {error}"))?;
    if read == 0 {
        return Err("HID feature report was empty".to_string());
    }
    response.truncate(read);
    Ok(response)
}

pub(super) fn get_input(
    device: &HidDevice,
    report_id: u8,
    length: usize,
) -> Result<Vec<u8>, String> {
    let mut response = vec![0; length];
    response[0] = report_id;
    let read = device
        .get_input_report(&mut response)
        .map_err(|error| format!("HID input report failed: {error}"))?;
    if read == 0 {
        return Err("HID input report was empty".to_string());
    }
    response.truncate(read);
    Ok(response)
}

pub(super) fn flush(device: &HidDevice) -> Result<(), String> {
    let mut buffer = [0; 128];
    loop {
        match device.read_timeout(&mut buffer, 5) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error) => return Err(format!("HID input flush failed: {error}")),
        }
    }
}

pub(super) fn percentage(byte: u8) -> Result<u8, String> {
    (byte <= 100)
        .then_some(byte)
        .ok_or_else(|| format!("invalid battery percentage {byte}"))
}

pub(super) fn map_battery(value: u8, minimum: u8, maximum: u8) -> Result<u8, String> {
    if maximum <= minimum || value < minimum {
        return Err(format!(
            "battery value {value} is outside {minimum}..={maximum}"
        ));
    }
    let value = value.min(maximum);
    let numerator = u16::from(value - minimum) * 100;
    let denominator = u16::from(maximum - minimum);
    Ok(u8::try_from(numerator / denominator).unwrap_or(100))
}

#[cfg(test)]
mod tests {
    use super::map_battery;

    #[test]
    fn maps_discrete_and_voltage_ranges() {
        assert_eq!(map_battery(2, 0, 4), Ok(50));
        assert_eq!(map_battery(91, 44, 91), Ok(100));
        assert!(map_battery(20, 44, 91).is_err());
    }
}
