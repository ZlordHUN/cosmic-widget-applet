// SPDX-License-Identifier: MPL-2.0

//! Native battery readers for Logitech gaming-headset-specific protocols.

use hidapi::HidDevice;

use super::Profile;
use super::transport::{QUERY_TIMEOUT_MS, Reading, percentage, read, write_padded};

const LOGITECH_VENDOR_ID: u16 = 0x046d;

const G533_CURVE: &[(u16, u8)] = &[
    (3_330, 0),
    (3_680, 5),
    (3_750, 20),
    (3_790, 30),
    (3_850, 50),
    (4_200, 100),
];
const G535_CURVE: &[(u16, u8)] = &[
    (3_310, 0),
    (3_664, 5),
    (3_730, 20),
    (3_766, 30),
    (3_817, 50),
    (4_175, 100),
];
const G633_CURVE: &[(u16, u8)] = &[
    (3_150, 0),
    (3_300, 5),
    (3_500, 10),
    (3_650, 20),
    (3_750, 40),
    (3_850, 60),
    (3_950, 80),
    (4_100, 100),
];
const GPRO_CURVE: &[(u16, u8)] = &[
    (3_320, 0),
    (3_670, 5),
    (3_740, 20),
    (3_780, 30),
    (3_830, 50),
    (4_150, 100),
];

pub(super) const PROFILES: &[Profile] = &[
    Profile {
        vendor_id: LOGITECH_VENDOR_ID,
        product_ids: &[0x0a66],
        name: "Logitech G533",
        interface: Some(3),
        query: query_g533,
    },
    Profile {
        vendor_id: LOGITECH_VENDOR_ID,
        product_ids: &[0x0ac4],
        name: "Logitech G535",
        interface: Some(3),
        query: query_g535,
    },
    Profile {
        vendor_id: LOGITECH_VENDOR_ID,
        product_ids: &[0x0a5c, 0x0a89, 0x0a5b, 0x0a87, 0x0ab5, 0x0afe, 0x0b1f],
        name: "Logitech G633/G635/G733/G933/G935",
        interface: None,
        query: query_g633_family,
    },
    Profile {
        vendor_id: LOGITECH_VENDOR_ID,
        product_ids: &[0x0aa7, 0x0aaa, 0x0aba, 0x0afb, 0x0afc],
        name: "Logitech G PRO Series",
        interface: None,
        query: query_gpro,
    },
    Profile {
        vendor_id: LOGITECH_VENDOR_ID,
        product_ids: &[0x0b18],
        name: "Logitech G522 LIGHTSPEED",
        interface: Some(3),
        query: query_g522,
    },
    Profile {
        vendor_id: LOGITECH_VENDOR_ID,
        product_ids: &[0x0af7],
        name: "Logitech G PRO X 2 LIGHTSPEED",
        interface: Some(3),
        query: query_gpro_x2_lightspeed,
    },
];

fn query_g533(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    query_voltage(device, [0x07, 0x01], G533_CURVE)
}

fn query_g535(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    query_voltage(device, [0x05, 0x0d], G535_CURVE)
}

fn query_g633_family(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    query_voltage(device, [0x08, 0x0a], G633_CURVE)
}

fn query_gpro(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    query_voltage(device, [0x06, 0x0d], GPRO_CURVE)
}

fn query_voltage(
    device: &HidDevice,
    command: [u8; 2],
    curve: &[(u16, u8)],
) -> Result<Reading, String> {
    write_padded(device, &[0x11, 0xff, command[0], command[1]], 20)?;
    let response = read(device, 7, QUERY_TIMEOUT_MS)?;
    if response.len() < 7 {
        return Err("Logitech headset battery response was too short".to_string());
    }
    if response[2] == 0xff {
        return Err("Logitech headset is offline".to_string());
    }
    if response[2] != command[0] || response[3] != command[1] {
        return Err("Logitech headset returned an unrelated response".to_string());
    }

    let voltage = u16::from_be_bytes([response[4], response[5]]);
    let level = interpolate_voltage(voltage, curve)
        .ok_or_else(|| format!("Logitech headset reported invalid voltage {voltage} mV"))?;
    Ok(if response[6] == 0x03 {
        Reading::charging(Some(level))
    } else {
        Reading::discharging(level)
    })
}

fn query_g522(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    let mut request = [0; 64];
    request[0] = 0x50;
    request[1] = 0x23;
    request[2] = 0x0b;
    request[4] = 0x03;
    request[5] = 0x1a;
    request[7] = 0x03;
    request[9] = 0x05;
    request[10] = 0x0a;
    write_padded(device, &request, 64)?;

    for _ in 0..4 {
        let response = read(device, 64, QUERY_TIMEOUT_MS)?;
        if response.len() >= 8 && response.starts_with(&[0x50, 0x23, 0x05]) && response[7] == 0 {
            return Err("Logitech G522 is offline".to_string());
        }
        if response.len() >= 14 && response.starts_with(&[0x50, 0x23, 0x0b]) && response[9] == 0x05
        {
            let level = percentage(response[11])?;
            return Ok(if response[13] == 0x02 {
                Reading::charging(Some(level))
            } else {
                Reading::discharging(level)
            });
        }
    }
    Err("Logitech G522 did not return a battery frame".to_string())
}

fn query_gpro_x2_lightspeed(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    let mut request = [0; 64];
    request[0] = 0x51;
    request[1] = 0x08;
    request[3] = 0x03;
    request[4] = 0x1a;
    request[6] = 0x03;
    request[8] = 0x04;
    request[9] = 0x0a;
    write_padded(device, &request, 64)?;

    for _ in 0..4 {
        let response = read(device, 64, QUERY_TIMEOUT_MS)?;
        if response.len() >= 7 && response.starts_with(&[0x51, 0x05]) && response[6] == 0 {
            return Err("Logitech G PRO X 2 LIGHTSPEED is offline".to_string());
        }
        if response.len() >= 13 && response.starts_with(&[0x51, 0x0b]) && response[8] == 0x04 {
            let level = percentage(response[10])?;
            return Ok(if response[12] == 0x02 {
                Reading::charging(Some(level))
            } else {
                Reading::discharging(level)
            });
        }
    }
    Err("Logitech G PRO X 2 LIGHTSPEED did not return a battery frame".to_string())
}

fn interpolate_voltage(voltage: u16, curve: &[(u16, u8)]) -> Option<u8> {
    let first = curve.first()?;
    let last = curve.last()?;
    if voltage < first.0 {
        return None;
    }
    if voltage >= last.0 {
        return Some(last.1);
    }

    curve.windows(2).find_map(|points| {
        let (low_voltage, low_level) = points[0];
        let (high_voltage, high_level) = points[1];
        if !(low_voltage..=high_voltage).contains(&voltage) {
            return None;
        }
        let span = u32::from(high_voltage - low_voltage);
        let offset = u32::from(voltage - low_voltage);
        let level_span = u32::from(high_level - low_level);
        Some(low_level + u8::try_from((offset * level_span + span / 2) / span).ok()?)
    })
}

#[cfg(test)]
mod tests {
    use super::{G533_CURVE, interpolate_voltage};

    #[test]
    fn interpolates_logitech_voltage_curve() {
        assert_eq!(interpolate_voltage(3_200, G533_CURVE), None);
        assert_eq!(interpolate_voltage(3_330, G533_CURVE), Some(0));
        assert_eq!(interpolate_voltage(3_750, G533_CURVE), Some(20));
        assert_eq!(interpolate_voltage(4_500, G533_CURVE), Some(100));
    }
}
