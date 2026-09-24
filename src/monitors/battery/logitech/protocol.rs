// SPDX-License-Identifier: MPL-2.0

//! Logitech HID++ battery feature and register decoding.

pub(super) const DEVICE_NAME_FEATURE: u16 = 0x0005;
pub(super) const DEVICE_FRIENDLY_NAME_FEATURE: u16 = 0x0007;

const BATTERY_STATUS_FEATURE: u16 = 0x1000;
const BATTERY_VOLTAGE_FEATURE: u16 = 0x1001;
const UNIFIED_BATTERY_FEATURE: u16 = 0x1004;
const ADC_MEASUREMENT_FEATURE: u16 = 0x1f20;
const CENTURION_BATTERY_FEATURE: u16 = 0x0104;

pub(super) const HIDPP10_BATTERY_STATUS_REGISTER: u16 = 0x07;
pub(super) const HIDPP10_BATTERY_CHARGE_REGISTER: u16 = 0x0d;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BatteryProtocol {
    Hidpp20 { feature: BatteryFeature, index: u8 },
    Hidpp10,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BatteryFeature {
    Status,
    Voltage,
    Unified,
    AdcMeasurement,
    Centurion,
}

impl BatteryFeature {
    pub(super) const ALL: [Self; 5] = [
        Self::Status,
        Self::Voltage,
        Self::Unified,
        Self::AdcMeasurement,
        Self::Centurion,
    ];

    pub(super) const fn id(self) -> u16 {
        match self {
            Self::Status => BATTERY_STATUS_FEATURE,
            Self::Voltage => BATTERY_VOLTAGE_FEATURE,
            Self::Unified => UNIFIED_BATTERY_FEATURE,
            Self::AdcMeasurement => ADC_MEASUREMENT_FEATURE,
            Self::Centurion => CENTURION_BATTERY_FEATURE,
        }
    }

    pub(super) const fn function(self) -> u8 {
        match self {
            Self::Unified => 0x10,
            Self::Status | Self::Voltage | Self::AdcMeasurement | Self::Centurion => 0x00,
        }
    }

    pub(super) fn parse(self, response: &[u8]) -> Result<BatteryReading, String> {
        match self {
            Self::Status => parse_battery_status(response),
            Self::Voltage => parse_battery_voltage(response),
            Self::Unified => parse_unified_battery(response),
            Self::AdcMeasurement => parse_adc_measurement(response),
            Self::Centurion => parse_centurion_battery(response),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BatteryReading {
    pub(super) level: Option<u8>,
    pub(super) status: Option<String>,
}

fn parse_battery_status(response: &[u8]) -> Result<BatteryReading, String> {
    let [discharge, _, status, ..] = response else {
        return Err("HID++ battery-status response was too short".to_string());
    };
    let status = parse_hidpp20_status(*status)
        .ok_or_else(|| "HID++ battery-status response had an unknown status".to_string())?;

    Ok(BatteryReading {
        level: reported_percentage(*discharge),
        status: Some(status),
    })
}

pub(super) fn parse_unified_battery(response: &[u8]) -> Result<BatteryReading, String> {
    let [discharge, approximation, status, ..] = response else {
        return Err("HID++ unified-battery response was too short".to_string());
    };
    let status = parse_hidpp20_status(*status)
        .ok_or_else(|| "HID++ unified-battery response had an unknown status".to_string())?;
    let level = reported_percentage(*discharge).or_else(|| match approximation {
        8 => Some(90),
        4 => Some(50),
        2 => Some(20),
        1 => Some(5),
        _ => None,
    });

    Ok(BatteryReading {
        level,
        status: Some(status),
    })
}

fn parse_battery_voltage(response: &[u8]) -> Result<BatteryReading, String> {
    let [voltage_high, voltage_low, flags, ..] = response else {
        return Err("HID++ battery-voltage response was too short".to_string());
    };
    let voltage = u16::from_be_bytes([*voltage_high, *voltage_low]);
    let status = if flags & 0x80 == 0 {
        Some("discharging".to_string())
    } else if flags & 0x03 == 0x01 {
        Some("charged".to_string())
    } else {
        Some("charging".to_string())
    };

    Ok(BatteryReading {
        level: estimate_battery_percentage(voltage),
        status,
    })
}

fn parse_adc_measurement(response: &[u8]) -> Result<BatteryReading, String> {
    let [voltage_high, voltage_low, flags, ..] = response else {
        return Err("HID++ ADC response was too short".to_string());
    };
    if flags & 0x01 == 0 {
        return Err("HID++ ADC battery reading was not valid".to_string());
    }
    let voltage = u16::from_be_bytes([*voltage_high, *voltage_low]);

    Ok(BatteryReading {
        level: estimate_battery_percentage(voltage),
        status: Some(if flags & 0x02 != 0 {
            "charging".to_string()
        } else {
            "discharging".to_string()
        }),
    })
}

fn parse_centurion_battery(response: &[u8]) -> Result<BatteryReading, String> {
    let Some(level) = response.first().copied().and_then(valid_percentage) else {
        return Err("Centurion battery response had no valid percentage".to_string());
    };
    let status = match response.get(2).copied().unwrap_or_default() {
        1 | 2 => "charging",
        3 => "charged",
        _ => "discharging",
    };

    Ok(BatteryReading {
        level: Some(level),
        status: Some(status.to_string()),
    })
}

pub(super) fn parse_hidpp10_battery(
    register: u16,
    response: &[u8],
) -> Result<BatteryReading, String> {
    match register {
        HIDPP10_BATTERY_CHARGE_REGISTER => {
            let [level, _, status, ..] = response else {
                return Err("HID++ 1.0 battery-charge response was too short".to_string());
            };
            let status = match status & 0xf0 {
                0x30 => Some("discharging".to_string()),
                0x50 => Some("charging".to_string()),
                0x90 => Some("charged".to_string()),
                _ => None,
            };
            Ok(BatteryReading {
                level: valid_percentage(*level),
                status,
            })
        }
        HIDPP10_BATTERY_STATUS_REGISTER => {
            let [level, charging, ..] = response else {
                return Err("HID++ 1.0 battery-status response was too short".to_string());
            };
            let status = if *charging == 0 {
                Some("discharging".to_string())
            } else if charging & 0x21 == 0x21 {
                Some("charging".to_string())
            } else if charging & 0x22 == 0x22 {
                Some("charged".to_string())
            } else {
                None
            };
            let level = match level {
                7 => Some(90),
                5 => Some(50),
                3 => Some(20),
                1 => Some(5),
                _ if charging & 0x03 != 0 => None,
                _ => Some(0),
            };
            Ok(BatteryReading { level, status })
        }
        _ => Err(format!(
            "unsupported HID++ 1.0 battery register {register:#04x}"
        )),
    }
}

fn parse_hidpp20_status(status: u8) -> Option<String> {
    match status {
        0x00 => Some("discharging".to_string()),
        0x01 | 0x02 | 0x04 => Some("charging".to_string()),
        0x03 => Some("charged".to_string()),
        0x05 => Some("invalid battery".to_string()),
        0x06 => Some("thermal error".to_string()),
        _ => None,
    }
}

fn valid_percentage(level: u8) -> Option<u8> {
    (level <= 100).then_some(level)
}

fn reported_percentage(level: u8) -> Option<u8> {
    (level <= 100 && level > 0).then_some(level)
}

fn estimate_battery_percentage(voltage: u16) -> Option<u8> {
    const VOLTAGE_CURVE: &[(u16, u8)] = &[
        (4186, 100),
        (4067, 90),
        (3989, 80),
        (3922, 70),
        (3859, 60),
        (3811, 50),
        (3778, 40),
        (3751, 30),
        (3717, 20),
        (3671, 10),
        (3646, 5),
        (3579, 2),
        (3500, 0),
    ];

    if voltage >= VOLTAGE_CURVE[0].0 {
        return Some(100);
    }
    if voltage <= VOLTAGE_CURVE.last()?.0 {
        return Some(0);
    }

    VOLTAGE_CURVE.windows(2).find_map(|window| {
        let (high_voltage, high_percentage) = window[0];
        let (low_voltage, low_percentage) = window[1];
        if !(low_voltage..=high_voltage).contains(&voltage) {
            return None;
        }

        let voltage_span = f32::from(high_voltage - low_voltage);
        let position = f32::from(voltage - low_voltage) / voltage_span;
        let percentage =
            f32::from(low_percentage) + f32::from(high_percentage - low_percentage) * position;
        Some(percentage.round() as u8)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        BatteryFeature, HIDPP10_BATTERY_CHARGE_REGISTER, HIDPP10_BATTERY_STATUS_REGISTER,
        estimate_battery_percentage, parse_hidpp10_battery, parse_unified_battery,
    };

    #[test]
    fn parses_every_hidpp20_battery_format() {
        assert_eq!(
            BatteryFeature::Status.parse(&[72, 60, 0]),
            Ok(super::BatteryReading {
                level: Some(72),
                status: Some("discharging".to_string()),
            })
        );
        assert_eq!(
            parse_unified_battery(&[0, 4, 1, 0]),
            Ok(super::BatteryReading {
                level: Some(50),
                status: Some("charging".to_string()),
            })
        );
        assert_eq!(
            BatteryFeature::Voltage.parse(&[0x0e, 0xe3, 0x80]),
            Ok(super::BatteryReading {
                level: estimate_battery_percentage(3811),
                status: Some("charging".to_string()),
            })
        );
        assert_eq!(
            BatteryFeature::AdcMeasurement.parse(&[0x0e, 0xe3, 0x01]),
            Ok(super::BatteryReading {
                level: Some(50),
                status: Some("discharging".to_string()),
            })
        );
        assert_eq!(
            BatteryFeature::Centurion.parse(&[96, 96, 2]),
            Ok(super::BatteryReading {
                level: Some(96),
                status: Some("charging".to_string()),
            })
        );
    }

    #[test]
    fn rejects_unified_battery_data_with_an_unknown_status() {
        assert!(parse_unified_battery(&[1, 1, 0xff, 0]).is_err());
    }

    #[test]
    fn parses_both_hidpp10_battery_registers() {
        assert_eq!(
            parse_hidpp10_battery(HIDPP10_BATTERY_CHARGE_REGISTER, &[83, 0, 0x50]),
            Ok(super::BatteryReading {
                level: Some(83),
                status: Some("charging".to_string()),
            })
        );
        assert_eq!(
            parse_hidpp10_battery(HIDPP10_BATTERY_STATUS_REGISTER, &[3, 0]),
            Ok(super::BatteryReading {
                level: Some(20),
                status: Some("discharging".to_string()),
            })
        );
    }

    #[test]
    fn interpolates_battery_voltage() {
        assert_eq!(estimate_battery_percentage(4186), Some(100));
        assert_eq!(estimate_battery_percentage(3811), Some(50));
        assert_eq!(estimate_battery_percentage(3500), Some(0));
        assert_eq!(estimate_battery_percentage(3794), Some(45));
    }
}
