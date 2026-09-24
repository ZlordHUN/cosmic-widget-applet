// SPDX-License-Identifier: MPL-2.0

//! Corsair wireless headset battery protocols.

use hidapi::HidDevice;

use super::Profile;
use super::transport::{QUERY_TIMEOUT_MS, Reading, flush, percentage, read, write, write_padded};

const CORSAIR_VENDOR_ID: u16 = 0x1b1c;
const VOID_PRODUCT_IDS: &[u16] = &[
    0x1b1c, 0x1b27, 0x0a14, 0x0a16, 0x0a17, 0x0a1d, 0x0a1a, 0x1b2a, 0x1b23, 0x1b29, 0x0a55, 0x0a51,
    0x0a52, 0x0a38, 0x0a4f, 0x0a2b, 0x0a75, 0x0a56,
];
const VOID_V2_PRODUCT_IDS: &[u16] = &[0x2a08, 0x2a02];

pub(super) const PROFILES: &[Profile] = &[
    Profile {
        vendor_id: CORSAIR_VENDOR_ID,
        product_ids: VOID_PRODUCT_IDS,
        name: "Corsair Headset Device",
        interface: Some(3),
        query: query_void,
    },
    Profile {
        vendor_id: CORSAIR_VENDOR_ID,
        product_ids: VOID_V2_PRODUCT_IDS,
        name: "Corsair Wireless V2 Headset Device",
        interface: Some(4),
        query: query_void_v2,
    },
];

fn query_void(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    write(device, &[0xc9, 0x64])?;
    parse_void(&read(device, 5, QUERY_TIMEOUT_MS)?)
}

fn parse_void(response: &[u8]) -> Result<Reading, String> {
    if response.len() < 5 {
        return Err("Corsair battery response was too short".to_string());
    }
    let status = response[4];
    if status == 0 {
        return Err("Corsair headset is offline".to_string());
    }
    if !matches!(status, 1 | 2 | 4 | 5) {
        return Err(format!("unknown Corsair battery status {status:#04x}"));
    }

    let level = percentage(response[2] & 0x7f)?;
    Ok(if matches!(status, 4 | 5) {
        Reading::charging(Some(level))
    } else {
        Reading::discharging(level)
    })
}

fn query_void_v2(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    initialize_void_v2(device)?;
    flush(device)?;

    let request = [0x00, 0x02, 0x09, 0x02, 0x0f];
    for _ in 0..2 {
        write_padded(device, &request, 65)?;
        let response = read(device, 64, QUERY_TIMEOUT_MS)?;
        if response.len() < 6 {
            return Err("Corsair V2 battery response was too short".to_string());
        }
        if response[5] == 0x2a {
            continue;
        }
        let raw = u16::from_le_bytes([response[4], response[5]]);
        return Ok(Reading::discharging(
            u8::try_from((raw / 10).min(100)).unwrap_or(100),
        ));
    }

    Err("Corsair V2 returned its receiver identifier instead of battery data".to_string())
}

fn initialize_void_v2(device: &HidDevice) -> Result<(), String> {
    const RECEIVER: u8 = 0x08;
    const HEADSET: u8 = 0x09;

    write_padded(device, &[0x00, 0x02, RECEIVER, 0x02, 0x13], 65)?;
    write_padded(device, &[0x00, 0x02, RECEIVER, 0x01, 0x03, 0x00, 0x02], 65)?;
    write_padded(device, &[0x00, 0x02, RECEIVER, 0x02, 0x12], 65)?;
    write_padded(device, &[0x00, 0x02, HEADSET, 0x01, 0x03, 0x00, 0x02], 65)?;
    flush(device)?;
    write_padded(device, &[0x00, 0x02, HEADSET, 0x02, 0x12], 65)?;
    read(device, 64, QUERY_TIMEOUT_MS).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::super::transport::Reading;
    use super::parse_void;

    #[test]
    fn parses_corsair_level_microphone_flag_and_charging() {
        assert_eq!(
            parse_void(&[100, 0, 0x80 | 73, 177, 5]),
            Ok(Reading::charging(Some(73)))
        );
        assert_eq!(
            parse_void(&[100, 0, 42, 177, 1]),
            Ok(Reading::discharging(42))
        );
        assert!(parse_void(&[100, 0, 42, 177, 0]).is_err());
    }
}
