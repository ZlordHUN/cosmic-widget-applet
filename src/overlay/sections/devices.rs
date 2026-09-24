// SPDX-License-Identifier: MPL-2.0

use crate::monitors::battery::BatteryDevice;
use crate::overlay::Message;
use crate::overlay::components::section::{METRIC_ICON_SIZE, section};
use crate::overlay::stats::SystemSnapshot;
use cosmic::iced::{Alignment, Length};
use cosmic::{Element, theme, widget};

pub(in crate::overlay) fn devices_view<'a>(
    stats: &'a SystemSnapshot,
    section_spacing: u16,
    row_spacing: u16,
) -> Element<'a, Message> {
    let mut devices = section(
        "preferences-input-devices-symbolic",
        "Devices",
        section_spacing,
    );

    if stats.devices.is_empty() {
        devices = devices.push(widget::text::caption("No battery devices found"));
    } else {
        for device in &stats.devices {
            devices = devices.push(device_item(device, row_spacing));
        }
    }

    devices.into()
}

fn device_item<'a>(device: &'a BatteryDevice, spacing: u16) -> Element<'a, Message> {
    widget::row::with_capacity(4)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(device_icon(device.kind.as_deref()))
        .push(widget::text::body(&device.name).width(Length::Fill))
        .push(battery_status(device, spacing))
        .into()
}

fn device_icon(kind: Option<&str>) -> Element<'static, Message> {
    let kind = kind.unwrap_or_default().to_ascii_lowercase();
    let icon = if kind.contains("mouse") {
        "input-mouse-symbolic"
    } else if kind.contains("keyboard") {
        "input-keyboard-symbolic"
    } else if kind.contains("headset") || kind.contains("headphone") {
        "audio-headset-symbolic"
    } else if kind.contains("controller") || kind.contains("gamepad") {
        "input-gaming-symbolic"
    } else {
        "preferences-input-devices-symbolic"
    };

    widget::icon::from_name(icon).size(METRIC_ICON_SIZE).into()
}

fn battery_status(device: &BatteryDevice, spacing: u16) -> Element<'static, Message> {
    let (icon, label, band, opacity) = battery_visuals(device);

    let battery_icon = widget::icon::from_name(icon)
        .icon()
        .size(METRIC_ICON_SIZE)
        .opacity(opacity)
        .class(theme::Svg::custom(move |theme| {
            let cosmic = theme.cosmic();
            let color = match band {
                BatteryBand::Success => cosmic.success_color(),
                BatteryBand::Warning => cosmic.warning_color(),
                BatteryBand::Destructive => cosmic.destructive_color(),
                BatteryBand::Cached => cosmic.accent_color(),
                BatteryBand::Unavailable => cosmic.on_bg_color(),
            };

            cosmic::iced::widget::svg::Style {
                color: Some(color.into()),
            }
        }));

    widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(spacing / 2)
        .push(battery_icon)
        .push(widget::text::monotext(label))
        .into()
}

fn battery_visuals(device: &BatteryDevice) -> (String, String, BatteryBand, f32) {
    if device.is_loading && device.level.is_some() {
        let level = device.level.unwrap_or_default();
        (
            battery_icon_name(level, is_charging(device.status.as_deref())),
            format!("{level}%"),
            BatteryBand::Cached,
            0.8,
        )
    } else if device.is_loading {
        (
            "battery-missing-symbolic".to_string(),
            "...".to_string(),
            BatteryBand::Unavailable,
            0.6,
        )
    } else if !device.is_connected {
        (
            "battery-missing-symbolic".to_string(),
            "N/A".to_string(),
            BatteryBand::Unavailable,
            0.6,
        )
    } else if let Some(level) = device.level {
        (
            battery_icon_name(level, is_charging(device.status.as_deref())),
            format!("{level}%"),
            battery_band(level),
            1.0,
        )
    } else {
        (
            "battery-missing-symbolic".to_string(),
            "N/A".to_string(),
            BatteryBand::Unavailable,
            0.6,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatteryBand {
    Success,
    Warning,
    Destructive,
    Cached,
    Unavailable,
}

fn battery_band(level: u8) -> BatteryBand {
    match level {
        0..=15 => BatteryBand::Destructive,
        16..=30 => BatteryBand::Warning,
        _ => BatteryBand::Success,
    }
}

fn battery_icon_name(level: u8, charging: bool) -> String {
    let bucket = match level.min(100) {
        0..=2 => 0,
        3..=7 => 5,
        8..=15 => 10,
        16..=27 => 20,
        28..=42 => 35,
        43..=57 => 50,
        58..=72 => 65,
        73..=85 => 80,
        86..=95 => 90,
        _ => 100,
    };
    let charging = if charging { "-charging" } else { "" };
    format!("cosmic-applet-battery-level-{bucket}{charging}-symbolic")
}

fn is_charging(status: Option<&str>) -> bool {
    status.is_some_and(|status| {
        let status = status.to_ascii_lowercase();
        status.starts_with("charging") || status.starts_with("recharging")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_visuals_follow_level_and_charging_state() {
        assert_eq!(battery_band(82), BatteryBand::Success);
        assert_eq!(battery_band(24), BatteryBand::Warning);
        assert_eq!(battery_band(10), BatteryBand::Destructive);
        assert_eq!(
            battery_icon_name(82, false),
            "cosmic-applet-battery-level-80-symbolic"
        );
        assert_eq!(
            battery_icon_name(69, true),
            "cosmic-applet-battery-level-65-charging-symbolic"
        );
        assert!(is_charging(Some("recharging")));
        assert!(!is_charging(Some("discharging")));
    }

    #[test]
    fn cached_battery_readings_use_the_accent_band_until_verified() {
        let device = BatteryDevice {
            name: "Test Headset".to_string(),
            level: Some(68),
            status: Some("charging".to_string()),
            kind: Some("headset".to_string()),
            codename: None,
            is_loading: true,
            is_connected: false,
        };

        let (icon, label, band, opacity) = battery_visuals(&device);

        assert_eq!(icon, "cosmic-applet-battery-level-65-charging-symbolic");
        assert_eq!(label, "68%");
        assert_eq!(band, BatteryBand::Cached);
        assert_eq!(opacity, 0.8);
    }
}
