// SPDX-License-Identifier: MPL-2.0

//! Native readers for headset protocols that do not form a larger family.

use std::thread;
use std::time::Duration;

use hidapi::HidDevice;

use super::Profile;
use super::transport::{
    QUERY_TIMEOUT_MS, Reading, get_feature, map_battery, percentage, read, send_feature,
    write_padded,
};

pub(super) const PROFILES: &[Profile] = &[
    Profile {
        vendor_id: 0x046d,
        product_ids: &[0x0b1c],
        name: "Logitech ASTRO A50 Gen 5",
        interface: Some(8),
        query: query_astro_a50,
    },
    Profile {
        vendor_id: 0x046d,
        product_ids: &[0x0a1f],
        name: "Logitech G930",
        interface: None,
        query: query_logitech_g930,
    },
    Profile {
        vendor_id: 0x17ef,
        product_ids: &[0xa07d],
        name: "Lenovo Wireless VoIP Headset",
        interface: Some(3),
        query: query_lenovo_voip,
    },
    Profile {
        vendor_id: 0x054c,
        product_ids: &[0x0ec2],
        name: "Sony INZONE Buds",
        interface: None,
        query: query_sony_inzone_buds,
    },
];

fn query_astro_a50(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    write_padded(device, &[0x02, 0x0c, 0x03, 0x00, 0x06, 0x0c], 64)?;
    for _ in 0..8 {
        let response = match read(device, 64, 100) {
            Ok(response) => response,
            Err(_) => continue,
        };
        if response.len() < 9 || response[0] != 0x02 || response[1] != 0x0c || response[4] != 0x06 {
            continue;
        }
        let level = percentage(response[6])?;
        return Ok(if response[8] != 0 {
            Reading::charging(Some(level))
        } else {
            Reading::discharging(level)
        });
    }
    Err("ASTRO A50 did not return a battery frame".to_string())
}

fn query_logitech_g930(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    let request = [
        0xff, 0x09, 0x00, 0xfd, 0xf4, 0x10, 0x05, 0xb1, 0xbf, 0xa0, 0x04,
    ];
    send_feature(device, &request, 64)?;

    let mut response = Vec::new();
    for attempt in 0..3 {
        response = get_feature(device, 0xff, 64)?;
        if attempt < 2 {
            thread::sleep(Duration::from_millis(100));
        }
    }
    let raw = response
        .get(13)
        .copied()
        .ok_or_else(|| "Logitech G930 battery response was too short".to_string())?;
    Ok(Reading::discharging(map_battery(raw, 44, 91)?))
}

fn query_lenovo_voip(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    send_feature(device, &[0x24, 0x01], 61)?;
    let response = read(device, 61, QUERY_TIMEOUT_MS)?;
    if response.len() != 61 || response[0] != 0x27 || response[1] != 0x01 {
        return Err("Lenovo VoIP headset returned an unrelated response".to_string());
    }
    if response[2] != 0 {
        return Err("Lenovo VoIP headset is offline".to_string());
    }
    Ok(Reading::discharging(percentage(response[7])?))
}

fn query_sony_inzone_buds(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    for _ in 0..12 {
        let response = match read(device, 64, 50) {
            Ok(response) => response,
            Err(_) => continue,
        };
        if response.len() < 19 || response[1] != 0x12 || response[2] != 0x04 {
            continue;
        }
        let right = percentage(response[14])?;
        let left = percentage(response[16])?;
        return Ok(Reading::discharging(left.min(right)));
    }
    Err("Sony INZONE Buds did not publish a battery frame".to_string())
}

#[cfg(test)]
mod tests {
    use super::super::transport::map_battery;

    #[test]
    fn maps_g930_raw_battery_range() {
        assert_eq!(map_battery(44, 44, 91), Ok(0));
        assert_eq!(map_battery(91, 44, 91), Ok(100));
    }
}
