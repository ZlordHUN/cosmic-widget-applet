// SPDX-License-Identifier: MPL-2.0

//! HyperX wireless headset battery protocols.

use std::thread;
use std::time::Duration;

use hidapi::HidDevice;

use super::Profile;
use super::transport::{
    QUERY_TIMEOUT_MS, Reading, get_input, percentage, read, write, write_padded,
};

const HP_VENDOR_ID: u16 = 0x03f0;
const KINGSTON_VENDOR_ID: u16 = 0x0951;

pub(super) const PROFILES: &[Profile] = &[
    Profile {
        vendor_id: HP_VENDOR_ID,
        product_ids: &[0x098d],
        name: "HyperX Cloud Alpha Wireless",
        interface: None,
        query: query_cloud_alpha,
    },
    Profile {
        vendor_id: KINGSTON_VENDOR_ID,
        product_ids: &[0x16c4, 0x1723],
        name: "HyperX Cloud Flight Wireless",
        interface: None,
        query: query_cloud_flight,
    },
    Profile {
        vendor_id: HP_VENDOR_ID,
        product_ids: &[0x0696],
        name: "HyperX Cloud II Wireless",
        interface: None,
        query: query_cloud_2_hp,
    },
    Profile {
        vendor_id: KINGSTON_VENDOR_ID,
        product_ids: &[0x1718],
        name: "HyperX Cloud II Wireless (Kingston)",
        interface: None,
        query: query_cloud_2_kingston,
    },
];

fn query_cloud_alpha(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    let connection = alpha_command(device, 0x03)?;
    if connection.get(3) == Some(&0x01) {
        return Err("HyperX Cloud Alpha is offline".to_string());
    }

    let charging = alpha_command(device, 0x0c)?;
    if charging.get(3) == Some(&0x01) {
        return Ok(Reading::charging(None));
    }

    let battery = alpha_command(device, 0x0b)?;
    let level = battery
        .get(3)
        .copied()
        .ok_or_else(|| "HyperX Cloud Alpha battery response was too short".to_string())
        .and_then(percentage)?;
    Ok(Reading::discharging(level))
}

fn alpha_command(device: &HidDevice, command: u8) -> Result<Vec<u8>, String> {
    write_padded(device, &[0x21, 0xbb, command], 31)?;
    read(device, 31, QUERY_TIMEOUT_MS)
}

fn query_cloud_flight(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    write_padded(device, &[0x21, 0xff, 0x05], 20)?;
    let response = read(device, 20, QUERY_TIMEOUT_MS)?;
    if !matches!(response.len(), 15 | 20) || response.len() < 5 {
        return Err("HyperX Cloud Flight battery response had an invalid length".to_string());
    }

    let voltage = u16::from_be_bytes([response[3], response[4]]);
    if voltage > 0x100b {
        return Ok(Reading::charging(None));
    }
    Ok(Reading::discharging(estimate_cloud_flight_level(voltage)))
}

fn estimate_cloud_flight_level(voltage: u16) -> u8 {
    if voltage <= 3648 {
        return (f64::from(voltage) * 0.00125).round().clamp(0.0, 100.0) as u8;
    }
    if voltage > 3975 {
        return 100;
    }

    let voltage = f64::from(voltage);
    (0.00000002547505 * voltage.powi(4) - 0.0003900299 * voltage.powi(3)
        + 2.238321 * voltage.powi(2)
        - 5706.256 * voltage
        + 5_452_299.0)
        .round()
        .clamp(0.0, 100.0) as u8
}

fn query_cloud_2_hp(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    let level = cloud_2_hp_command(device, 0x02)?;
    let charging = cloud_2_hp_command(device, 0x03)?;
    let percentage = level
        .get(7)
        .copied()
        .ok_or_else(|| "HyperX Cloud II battery response was too short".to_string())
        .and_then(percentage)?;
    Ok(if charging.get(4) == Some(&0x01) {
        Reading::charging(Some(percentage))
    } else {
        Reading::discharging(percentage)
    })
}

fn cloud_2_hp_command(device: &HidDevice, command: u8) -> Result<Vec<u8>, String> {
    write_padded(device, &[0x06, 0xff, 0xbb, command, 0x00], 52)?;
    thread::sleep(Duration::from_millis(100));
    let response = read(device, 20, 1_000)?;
    if response.len() != 20 || response[..4] != [0x06, 0xff, 0xbb, command] {
        return Err("HyperX Cloud II returned an unrelated response".to_string());
    }
    Ok(response)
}

fn query_cloud_2_kingston(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    let level = cloud_2_kingston_command(device, 0x02)?;
    let charging = cloud_2_kingston_command(device, 0x03)?;
    let percentage = level
        .get(7)
        .copied()
        .ok_or_else(|| "Kingston Cloud II battery response was too short".to_string())
        .and_then(percentage)?;
    Ok(if charging.get(4) == Some(&0x01) {
        Reading::charging(Some(percentage))
    } else {
        Reading::discharging(percentage)
    })
}

fn cloud_2_kingston_command(device: &HidDevice, command: u8) -> Result<Vec<u8>, String> {
    let _ = get_input(device, 0x06, 64);

    let mut request = vec![0; 62];
    request[..17].copy_from_slice(&[
        0x06, 0x00, 0x02, 0x00, 0x9a, 0x00, 0x00, 0x68, 0x4a, 0x8e, 0x0a, 0x00, 0x00, 0x00, 0xbb,
        command, 0x00,
    ]);
    write(device, &request)?;
    thread::sleep(Duration::from_millis(100));

    let response = read(device, 64, 1_000)?;
    if response.len() < 8 || response[0] != 0x0b || response[2] != 0xbb || response[3] != command {
        return Err("Kingston Cloud II returned an unrelated response".to_string());
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::estimate_cloud_flight_level;

    #[test]
    fn cloud_flight_curve_is_bounded() {
        for voltage in 0..=u16::MAX {
            assert!(estimate_cloud_flight_level(voltage) <= 100);
        }
        assert_eq!(estimate_cloud_flight_level(4_100), 100);
    }
}
