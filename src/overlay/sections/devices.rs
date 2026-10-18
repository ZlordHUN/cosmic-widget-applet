// SPDX-License-Identifier: MPL-2.0

use crate::monitors::battery::BatteryDevice;
use crate::overlay::Message;
use crate::overlay::components::section::{METRIC_ICON_SIZE, section};
use crate::overlay::components::{battery_icon, reveal};
use crate::overlay::device_state::DeviceList;
use cosmic::iced::{Alignment, Color, Length, Padding};
use cosmic::{Element, Theme, theme, widget};

// COSMIC body text defines the 21-pixel row; icons and monospace labels fit inside.
const DEVICE_ROW_HEIGHT: f32 = 21.0;

pub(in crate::overlay) fn reveal_row_height(section_spacing: u16) -> f32 {
    DEVICE_ROW_HEIGHT + f32::from(section_spacing)
}

pub(in crate::overlay) fn devices_view<'a>(
    state: &'a DeviceList,
    section_spacing: u16,
    row_spacing: u16,
) -> Element<'a, Message> {
    // The gap belongs to the revealed row so removing it leaves no empty space.
    let mut devices = section("preferences-input-devices-symbolic", "Devices", 0);
    let padding = Padding {
        top: f32::from(section_spacing),
        ..Padding::ZERO
    };
    for row in state.rows() {
        devices = devices.push(reveal::vertical(
            widget::container(device_item(&row.device, row_spacing))
                .width(Length::Fill)
                .padding(padding)
                .into(),
            row.animation.progress,
        ));
    }
    if state.empty_progress() > 0.0 {
        devices = devices.push(reveal::vertical(
            widget::container(
                widget::container(widget::text::caption("No battery devices found"))
                    .height(Length::Fixed(DEVICE_ROW_HEIGHT))
                    .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding(padding)
            .into(),
            state.empty_progress(),
        ));
    }
    devices.into()
}

fn device_item<'a>(device: &'a BatteryDevice, spacing: u16) -> Element<'a, Message> {
    widget::row::with_capacity(4)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(device_icon(device.kind.as_deref()))
        .push(widget::tooltip(
            widget::text::body(&device.name)
                .width(Length::Fill)
                .wrapping(cosmic::iced::widget::text::Wrapping::None)
                .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                    cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                )),
            widget::text::caption(&device.name),
            widget::tooltip::Position::Top,
        ))
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

    let battery_icon = if let Some(level) = device
        .level
        .filter(|_| device.is_connected || device.is_loading)
    {
        let mode = if live_charging_level(device).is_some() {
            battery_icon::Mode::Charging
        } else if is_charging(device.status.as_deref()) {
            battery_icon::Mode::CachedCharging
        } else {
            battery_icon::Mode::Level
        };
        battery_icon::battery(level, mode, METRIC_ICON_SIZE, move |theme| {
            let mut color = band_color(theme, band);
            color.a *= opacity;
            color
        })
    } else {
        widget::icon::from_name(icon)
            .icon()
            .size(METRIC_ICON_SIZE)
            .opacity(opacity)
            .class(theme::Svg::custom(move |theme| {
                cosmic::iced::widget::svg::Style {
                    color: Some(band_color(theme, band)),
                }
            }))
            .into()
    };

    widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(spacing / 2)
        .push(widget::container(battery_icon).center_x(battery_icon::width(METRIC_ICON_SIZE)))
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

fn band_color(theme: &Theme, band: BatteryBand) -> Color {
    let cosmic = theme.cosmic();
    match band {
        BatteryBand::Success => cosmic.success_color(),
        BatteryBand::Warning => cosmic.warning_color(),
        BatteryBand::Destructive => cosmic.destructive_color(),
        BatteryBand::Cached => cosmic.accent_color(),
        BatteryBand::Unavailable => cosmic.on_bg_color(),
    }
    .into()
}

/// Live charging readings animate. Cached ones keep the static glyph until the
/// device confirms it is still plugged in.
fn live_charging_level(device: &BatteryDevice) -> Option<u8> {
    device.level.filter(|_| {
        device.is_connected && !device.is_loading && is_charging(device.status.as_deref())
    })
}

fn battery_icon_name(level: u8, charging: bool) -> String {
    let bucket = level_bucket(level);
    let charging = if charging { "-charging" } else { "" };
    format!("cosmic-applet-battery-level-{bucket}{charging}-symbolic")
}

/// The step of the COSMIC battery icon that represents `level`.
fn level_bucket(level: u8) -> u8 {
    match level.min(100) {
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
    }
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

    #[tokio::test]
    async fn device_card_height_tracks_reveal_and_collapse_without_row_gaps() {
        use crate::overlay::animation::NOTIFICATION_GROUP_EXPANSION_DURATION;
        use crate::overlay::components::section::{CARD_VERTICAL_PADDING, section_card};
        use crate::overlay::layout::{MAX_VISIBLE_NOTIFICATION_ROWS, NOTIFICATION_ITEM_HEIGHT};
        use cosmic::iced::advanced::renderer::Headless as _;
        use cosmic::iced::advanced::{Layout, Renderer as _, layout, renderer, widget::Tree};
        use cosmic::iced::{Font, Pixels, Rectangle, Size, mouse};
        use std::time::Instant;

        let mut renderer = <cosmic::Renderer as renderer::Headless>::new(
            Font::default(),
            Pixels(14.0),
            Some("tiny-skia"),
        )
        .await
        .expect("software renderer for device animation validation");
        let devices: Vec<_> = [
            ("G309 LIGHTSPEED", "mouse"),
            ("MX Mechanical Mini", "keyboard"),
            ("Audeze Maxwell", "headset"),
            ("Razer Wolverine V3 Pro 8K PC", "controller"),
        ]
        .into_iter()
        .map(|(name, kind)| BatteryDevice {
            name: name.into(),
            kind: Some(kind.into()),
            level: Some(75),
            status: Some("discharging".into()),
            codename: None,
            is_loading: false,
            is_connected: true,
        })
        .collect();
        let row_height = reveal_row_height(8);
        let duration = NOTIFICATION_GROUP_EXPANSION_DURATION.mul_f32(
            row_height / (NOTIFICATION_ITEM_HEIGHT * MAX_VISIBLE_NOTIFICATION_ROWS) as f32,
        );
        let started = Instant::now();
        let theme = Theme::dark();
        let mut gallery = image::RgbaImage::new(346 * 5, 200 * 2);
        for (direction, connecting) in [true, false].into_iter().enumerate() {
            for frame in 0..=4 {
                let progress = frame as f32 / 4.0;
                let mut state = DeviceList::default();
                let (before, after) = if connecting {
                    (&devices[..3], &devices[..])
                } else {
                    (&devices[..], &devices[..3])
                };
                state.reconcile(before, started, row_height);
                state.reconcile(after, started, row_height);
                state.advance(started + duration.mul_f32(progress));
                let visible = if connecting { progress } else { 1.0 - progress };
                let expected =
                    21.0 + 2.0 * f32::from(CARD_VERTICAL_PADDING) + (3.0 + visible) * row_height;
                let mut element = section_card(devices_view(&state, 8, 8), 12);
                let mut tree = Tree::new(element.as_widget());
                let node = element.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(346.0, 200.0)),
                );
                assert!(
                    (node.size().height - expected).abs() < 0.1,
                    "connecting={connecting} frame={frame}: {} != {expected}",
                    node.size().height
                );
                // Optional preview uses the same real renderer and state as the widget.
                if std::env::var_os("COSMIC_DEVICE_REVEAL_PREVIEW").is_some() {
                    let viewport = Rectangle::with_size(Size::new(346.0, 200.0));
                    renderer.reset(viewport);
                    element.as_widget().draw(
                        &tree,
                        &mut renderer,
                        &theme,
                        &renderer::Style {
                            text_color: Color::WHITE,
                            icon_color: Color::WHITE,
                            scale_factor: 1.0,
                        },
                        Layout::new(&node),
                        mouse::Cursor::Unavailable,
                        &viewport,
                    );
                    let pixels = renderer.screenshot(
                        Size::new(346, 200),
                        1.0,
                        theme.cosmic().background(false).base.into(),
                    );
                    let frame_image = image::RgbaImage::from_raw(346, 200, pixels).unwrap();
                    image::imageops::replace(
                        &mut gallery,
                        &frame_image,
                        i64::from(frame * 346),
                        direction as i64 * 200,
                    );
                }
            }
        }
        if let Some(path) = std::env::var_os("COSMIC_DEVICE_REVEAL_PREVIEW") {
            gallery.save(path).unwrap();
        }
    }

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

    #[test]
    fn only_live_charging_readings_animate() {
        let charging = BatteryDevice {
            name: "Test Mouse".to_string(),
            level: Some(68),
            status: Some("charging".to_string()),
            kind: Some("mouse".to_string()),
            codename: None,
            is_loading: false,
            is_connected: true,
        };

        assert_eq!(live_charging_level(&charging), Some(68));
        for level in [96, 99, 100] {
            let near_full = BatteryDevice {
                level: Some(level),
                ..charging.clone()
            };
            assert_eq!(level_bucket(level), 100);
            assert_eq!(live_charging_level(&near_full), Some(level));
            assert_eq!(battery_visuals(&near_full).1, format!("{level}%"));
            assert_eq!(
                live_charging_level(&BatteryDevice {
                    status: Some("charged".to_string()),
                    ..near_full
                }),
                None
            );
        }
        for static_reading in [
            BatteryDevice {
                is_loading: true,
                ..charging.clone()
            },
            BatteryDevice {
                is_connected: false,
                ..charging.clone()
            },
            BatteryDevice {
                status: Some("charged".to_string()),
                ..charging.clone()
            },
            BatteryDevice {
                level: None,
                ..charging
            },
        ] {
            assert_eq!(live_charging_level(&static_reading), None);
        }
    }
}
