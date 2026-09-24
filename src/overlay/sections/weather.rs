// SPDX-License-Identifier: MPL-2.0

use crate::monitors::weather::WeatherData;
use crate::overlay::Message;
use crate::overlay::components::section::section;
use crate::overlay::stats::SystemSnapshot;
use cosmic::iced::{Alignment, Background, Border, Color, ContentFit, Length};
use cosmic::{Element, theme, widget};

pub(in crate::overlay) fn weather_view<'a>(
    stats: &'a SystemSnapshot,
    section_spacing: u16,
    content_spacing: u16,
    detail_spacing: u16,
) -> Element<'a, Message> {
    let mut weather = section("weather-symbolic", "Weather", section_spacing);

    if let Some(data) = &stats.weather {
        weather = weather.push(weather_content(data, content_spacing, detail_spacing));
    } else {
        weather = weather.push(widget::text::caption("Retrieving weather..."));
    }

    weather.into()
}

fn weather_content<'a>(
    data: &'a WeatherData,
    content_spacing: u16,
    detail_spacing: u16,
) -> Element<'a, Message> {
    let temperature = widget::row::with_capacity(3)
        .align_y(Alignment::Center)
        .spacing(detail_spacing)
        .push(widget::text::title4(format_weather_temperature(
            data.temperature,
        )))
        .push(widget::space::horizontal())
        .push(feels_like_badge(data.feels_like));

    let details = widget::column::with_capacity(3)
        .width(Length::Fill)
        .spacing(detail_spacing)
        .push(temperature)
        .push(widget::text::body(&data.description))
        .push(widget::text::caption(&data.location));

    widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(content_spacing)
        .push(weather_condition_icon(&data.icon))
        .push(details)
        .into()
}

fn weather_condition_icon(icon: &str) -> Element<'static, Message> {
    let mut handle = widget::icon::from_name(weather_icon_name(icon)).handle();
    handle.symbolic = true;
    // Some theme icons are wider than they are tall (the COSMIC rain icon is 17×16).
    // Preserve that aspect ratio so the SVG renderer does not clip them to a square.
    widget::icon::icon(handle)
        .size(64)
        .content_fit(ContentFit::Contain)
        .into()
}

fn feels_like_badge(value: f32) -> Element<'static, Message> {
    widget::container(widget::text::caption(format!(
        "Feels like {}",
        format_weather_temperature(value)
    )))
    .padding([4, 8])
    .class(theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let warning: Color = cosmic.warning_color().into();
        let background = Color { a: 0.16, ..warning };
        let border = Color { a: 0.7, ..warning };

        cosmic::iced::widget::container::Style {
            text_color: Some(warning),
            background: Some(Background::Color(background)),
            border: Border {
                color: border,
                width: 1.0,
                radius: cosmic.corner_radii.radius_s.into(),
            },
            ..Default::default()
        }
    }))
    .into()
}

fn weather_icon_name(icon: &str) -> &'static str {
    match icon {
        "01d" => "weather-clear-symbolic",
        "01n" => "weather-clear-night-symbolic",
        "02d" => "weather-few-clouds-symbolic",
        "02n" => "weather-few-clouds-night-symbolic",
        "03d" | "03n" | "04d" | "04n" => "weather-overcast-symbolic",
        "09d" | "09n" | "10d" | "10n" => "weather-showers-symbolic",
        "11d" | "11n" => "weather-storm-symbolic",
        "13d" | "13n" => "weather-snow-symbolic",
        "50d" | "50n" => "weather-fog-symbolic",
        _ => "weather-severe-alert-symbolic",
    }
}

fn format_weather_temperature(value: f32) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract().abs() < f32::EPSILON {
        format!("{rounded:.0}\u{b0}C")
    } else {
        format!("{rounded:.1}\u{b0}C")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_visuals_use_cosmic_condition_icons_and_compact_units() {
        assert_eq!(weather_icon_name("01n"), "weather-clear-night-symbolic");
        assert_eq!(weather_icon_name("02d"), "weather-few-clouds-symbolic");
        assert_eq!(weather_icon_name("04d"), "weather-overcast-symbolic");
        assert_eq!(weather_icon_name("13d"), "weather-snow-symbolic");
        assert_eq!(format_weather_temperature(3.2), "3.2\u{b0}C");
        assert_eq!(format_weather_temperature(1.0), "1\u{b0}C");
    }
}
