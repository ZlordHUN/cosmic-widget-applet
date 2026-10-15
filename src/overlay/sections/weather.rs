// SPDX-License-Identifier: MPL-2.0

use crate::monitors::weather::WeatherData;
use crate::overlay::Message;
use crate::overlay::components::{section::section, weather_icon::weather_icon};
use crate::overlay::stats::SystemSnapshot;
use cosmic::iced::{Alignment, Background, Border, Color, Length};
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
        .push(weather_icon(&data.icon))
        .push(details)
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
    fn weather_temperature_uses_compact_units() {
        assert_eq!(format_weather_temperature(3.2), "3.2\u{b0}C");
        assert_eq!(format_weather_temperature(1.0), "1\u{b0}C");
    }
}
