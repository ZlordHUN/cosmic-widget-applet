// SPDX-License-Identifier: MPL-2.0

//! SteelSeries legacy and Nova wireless battery protocols.

use hidapi::HidDevice;

use super::Profile;
use super::transport::{
    QUERY_TIMEOUT_MS, Reading, map_battery, percentage, read, send_feature, write, write_padded,
};

const STEELSERIES_VENDOR_ID: u16 = 0x1038;

pub(super) const PROFILES: &[Profile] = &[
    Profile {
        vendor_id: STEELSERIES_VENDOR_ID,
        product_ids: &[0x12b3, 0x12b6, 0x12d7, 0x12d5],
        name: "SteelSeries Arctis (1/7X/7P) Wireless",
        interface: Some(3),
        query: query_arctis_1,
    },
    Profile {
        vendor_id: STEELSERIES_VENDOR_ID,
        product_ids: &[0x1260, 0x12ad, 0x1252, 0x1280],
        name: "SteelSeries Arctis (7/Pro)",
        interface: Some(5),
        query: query_arctis_7,
    },
    Profile {
        vendor_id: STEELSERIES_VENDOR_ID,
        product_ids: &[0x220e, 0x2212, 0x2216, 0x2236],
        name: "SteelSeries Arctis 7+",
        interface: Some(3),
        query: query_arctis_7_plus,
    },
    Profile {
        vendor_id: STEELSERIES_VENDOR_ID,
        product_ids: &[0x12c2],
        name: "SteelSeries Arctis 9",
        interface: None,
        query: query_arctis_9,
    },
    Profile {
        vendor_id: STEELSERIES_VENDOR_ID,
        product_ids: &[0x1290],
        name: "SteelSeries Arctis Pro Wireless",
        interface: None,
        query: query_arctis_pro_wireless,
    },
    Profile {
        vendor_id: STEELSERIES_VENDOR_ID,
        product_ids: &[0x12e0, 0x12e5],
        name: "SteelSeries Arctis Nova Pro Wireless",
        interface: Some(4),
        query: query_nova_pro_wireless,
    },
    Profile {
        vendor_id: STEELSERIES_VENDOR_ID,
        product_ids: &[
            0x2202, 0x22a1, 0x227e, 0x2206, 0x2258, 0x229e, 0x22ad, 0x223a, 0x22a9, 0x227a, 0x22a4,
            0x22a5,
        ],
        name: "SteelSeries Arctis Nova 7",
        interface: Some(3),
        query: query_nova_7,
    },
    Profile {
        vendor_id: STEELSERIES_VENDOR_ID,
        product_ids: &[0x220a, 0x22a7],
        name: "SteelSeries Arctis Nova 7P",
        interface: Some(3),
        query: query_nova_7,
    },
    Profile {
        vendor_id: STEELSERIES_VENDOR_ID,
        product_ids: &[0x2232, 0x2253],
        name: "SteelSeries Arctis Nova (5/5X)",
        interface: Some(3),
        query: query_nova_5,
    },
    Profile {
        vendor_id: STEELSERIES_VENDOR_ID,
        product_ids: &[0x2269, 0x226d],
        name: "SteelSeries Arctis Nova 3P Wireless",
        interface: Some(3),
        query: query_nova_3p,
    },
    Profile {
        vendor_id: STEELSERIES_VENDOR_ID,
        product_ids: &[0x230a],
        name: "SteelSeries Arctis GameBuds",
        interface: Some(3),
        query: query_gamebuds,
    },
];

struct DirectBattery {
    response_size: usize,
    battery_index: usize,
    status_index: Option<usize>,
    offline_value: Option<u8>,
    charging_value: Option<u8>,
    minimum: u8,
    maximum: u8,
}

fn query_direct(
    device: &HidDevice,
    request: &[u8],
    spec: DirectBattery,
) -> Result<Reading, String> {
    write(device, request)?;
    parse_direct(&read(device, spec.response_size, QUERY_TIMEOUT_MS)?, spec)
}

fn parse_direct(response: &[u8], spec: DirectBattery) -> Result<Reading, String> {
    let raw = response
        .get(spec.battery_index)
        .copied()
        .ok_or_else(|| "SteelSeries battery response was too short".to_string())?;
    let status = spec
        .status_index
        .and_then(|index| response.get(index))
        .copied();
    if status
        .zip(spec.offline_value)
        .is_some_and(|(status, offline)| status == offline)
    {
        return Err("SteelSeries headset is offline".to_string());
    }

    let level = map_battery(raw, spec.minimum, spec.maximum)?;
    Ok(
        if status
            .zip(spec.charging_value)
            .is_some_and(|(status, charging)| status == charging)
        {
            Reading::charging(Some(level))
        } else {
            Reading::discharging(level)
        },
    )
}

fn query_arctis_1(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    query_direct(
        device,
        &[0x06, 0x12],
        DirectBattery {
            response_size: 8,
            battery_index: 3,
            status_index: Some(2),
            offline_value: Some(0x01),
            charging_value: None,
            minimum: 0,
            maximum: 100,
        },
    )
}

fn query_arctis_7(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    query_direct(
        device,
        &[0x06, 0x18],
        DirectBattery {
            response_size: 8,
            battery_index: 2,
            status_index: None,
            offline_value: None,
            charging_value: None,
            minimum: 0,
            maximum: 100,
        },
    )
}

fn query_arctis_9(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    query_direct(
        device,
        &[0x00, 0x20],
        DirectBattery {
            response_size: 12,
            battery_index: 3,
            status_index: Some(4),
            offline_value: None,
            charging_value: Some(0x01),
            minimum: 0x64,
            maximum: 0x9a,
        },
    )
}

fn nova_status(device: &HidDevice) -> Result<Vec<u8>, String> {
    write(device, &[0x00, 0xb0])?;
    read(device, 128, QUERY_TIMEOUT_MS)
}

fn query_arctis_7_plus(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    let response = nova_status(device)?;
    if response.len() < 4 {
        return Err("Arctis 7+ status response was too short".to_string());
    }
    if response[1] == 0x01 {
        return Err("Arctis 7+ is offline".to_string());
    }
    let level = map_battery(response[2], 0, 4)?;
    Ok(if response[3] == 0x01 {
        Reading::charging(Some(level))
    } else {
        Reading::discharging(level)
    })
}

fn query_nova_7(device: &HidDevice, product_id: u16) -> Result<Reading, String> {
    parse_nova_7(&nova_status(device)?, product_id)
}

fn parse_nova_7(response: &[u8], product_id: u16) -> Result<Reading, String> {
    if response.len() < 4 {
        return Err("Arctis Nova 7 status response was too short".to_string());
    }
    if response[3] == 0 {
        return Err("Arctis Nova 7 is offline".to_string());
    }
    let discrete = matches!(
        product_id,
        0x2202 | 0x2206 | 0x220a | 0x223a | 0x227a | 0x22a4
    );
    let level = if discrete {
        map_battery(response[2], 0, 4)?
    } else {
        percentage(response[2])?
    };
    Ok(if matches!(response[3], 0x01 | 0x02) {
        Reading::charging(Some(level))
    } else {
        Reading::discharging(level)
    })
}

fn query_nova_5(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    let response = nova_status(device)?;
    if response.len() < 5 {
        return Err("Arctis Nova 5 status response was too short".to_string());
    }
    if response[1] == 0x02 {
        return Err("Arctis Nova 5 is offline".to_string());
    }
    let level = percentage(response[3])?;
    Ok(if response[4] == 0x01 {
        Reading::charging(Some(level))
    } else {
        Reading::discharging(level)
    })
}

fn query_nova_3p(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    send_feature(device, &[0xb0], 64)?;
    let response = read(device, 4, QUERY_TIMEOUT_MS)?;
    if response.len() < 4 {
        return Err("Arctis Nova 3P status response was too short".to_string());
    }
    if response[1] == 0x02 {
        return Err("Arctis Nova 3P is offline".to_string());
    }
    Ok(Reading::discharging(percentage(response[3])?))
}

fn query_gamebuds(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    let response = nova_status(device)?;
    if response.len() < 7 {
        return Err("Arctis GameBuds status response was too short".to_string());
    }
    let mut levels = Vec::with_capacity(2);
    if response[3] == 0x03 {
        levels.push(percentage(response[5])?);
    }
    if response[4] == 0x03 {
        levels.push(percentage(response[6])?);
    }
    levels
        .into_iter()
        .min()
        .map(Reading::discharging)
        .ok_or_else(|| "Arctis GameBuds are docked".to_string())
}

fn query_arctis_pro_wireless(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    write_padded(device, &[0x41, 0xaa], 31)?;
    let status = read(device, 2, QUERY_TIMEOUT_MS)?;
    if status.first() == Some(&0x02) {
        return Err("Arctis Pro Wireless is offline".to_string());
    }

    write_padded(device, &[0x40, 0xaa], 31)?;
    let battery = read(device, 1, QUERY_TIMEOUT_MS)?;
    let level = battery
        .first()
        .copied()
        .ok_or_else(|| "Arctis Pro Wireless battery response was empty".to_string())
        .and_then(|raw| map_battery(raw, 0, 4))?;
    Ok(Reading::discharging(level))
}

fn query_nova_pro_wireless(device: &HidDevice, _product_id: u16) -> Result<Reading, String> {
    write_padded(device, &[0x06, 0xb0], 31)?;
    let response = read(device, 128, QUERY_TIMEOUT_MS)?;
    if response.len() < 16 {
        return Err("Arctis Nova Pro Wireless response was too short".to_string());
    }
    if response[15] == 0x01 {
        return Err("Arctis Nova Pro Wireless is offline".to_string());
    }
    let level = map_battery(response[6], 0, 8)?;
    Ok(if response[15] == 0x02 {
        Reading::charging(Some(level))
    } else {
        Reading::discharging(level)
    })
}

#[cfg(test)]
mod tests {
    use super::super::transport::Reading;
    use super::{DirectBattery, parse_direct, parse_nova_7};

    #[test]
    fn parses_legacy_direct_battery_and_offline_status() {
        let spec = || DirectBattery {
            response_size: 8,
            battery_index: 3,
            status_index: Some(2),
            offline_value: Some(1),
            charging_value: None,
            minimum: 0,
            maximum: 100,
        };
        assert_eq!(
            parse_direct(&[0, 0, 0, 73], spec()),
            Ok(Reading::discharging(73))
        );
        assert!(parse_direct(&[0, 0, 1, 73], spec()).is_err());
    }

    #[test]
    fn distinguishes_discrete_and_percentage_nova_7_models() {
        assert_eq!(
            parse_nova_7(&[0, 0, 3, 8], 0x2202),
            Ok(Reading::discharging(75))
        );
        assert_eq!(
            parse_nova_7(&[0, 0, 83, 8], 0x22a1),
            Ok(Reading::discharging(83))
        );
    }
}
