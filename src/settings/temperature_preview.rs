// SPDX-License-Identifier: MPL-2.0

//! Temperature gauge style cards and their canvas previews.

use super::{Message, SettingsApp};
use crate::config::TemperatureGaugeStyle;
use cosmic::Element;
use cosmic::iced::widget::canvas;
use cosmic::iced::{Color, Length, Point, Radians, Rectangle, Size, mouse};
use cosmic::widget;
use std::f32::consts::PI;

const TEMPERATURE_STYLE_PREVIEW_HEIGHT: f32 = 104.0;

impl SettingsApp {
    pub(super) fn temperature_style_selector(&self) -> Element<'_, Message> {
        let options = widget::row::with_capacity(3)
            .spacing(16)
            .width(Length::Fill)
            .push(self.temperature_style_card(TemperatureGaugeStyle::Arc, "Arc"))
            .push(self.temperature_style_card(TemperatureGaugeStyle::Circular, "Circular"))
            .push(self.temperature_style_card(TemperatureGaugeStyle::Text, "Text"));

        widget::column::with_capacity(2)
            .spacing(8)
            .push(widget::text::heading("Gauge style"))
            .push(options)
            .into()
    }

    fn temperature_style_card(
        &self,
        style: TemperatureGaugeStyle,
        label: &'static str,
    ) -> Element<'_, Message> {
        let preview = cosmic::iced::widget::Canvas::new(TemperatureStylePreview { style })
            .width(Length::Fill)
            .height(Length::Fixed(TEMPERATURE_STYLE_PREVIEW_HEIGHT));
        let selected = self.config.temperature_gauge_style == style;
        let button = widget::button::custom_image_button(preview, None::<Message>)
            .class(cosmic::theme::Button::Image)
            .selected(selected)
            .padding(0)
            .width(Length::Fill)
            .height(Length::Fixed(TEMPERATURE_STYLE_PREVIEW_HEIGHT))
            .on_press(Message::SetTemperatureGaugeStyle(style));

        widget::column::with_capacity(2)
            .spacing(6)
            .width(Length::FillPortion(1))
            .push(button)
            .push(widget::container(widget::text::body(label)).center_x(Length::Fill))
            .into()
    }
}

#[derive(Debug, Clone, Copy)]
struct TemperatureStylePreview {
    style: TemperatureGaugeStyle,
}

impl canvas::Program<Message, cosmic::Theme, cosmic::Renderer> for TemperatureStylePreview {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &cosmic::Renderer,
        theme: &cosmic::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry<cosmic::Renderer>> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let cosmic = theme.cosmic();
        let background: Color = theme.current_container().component.base.into();
        let track: Color = theme.current_container().component.on_disabled.into();
        let active: Color = cosmic.accent.base.into();
        let preview = canvas::Path::rounded_rectangle(
            Point::ORIGIN,
            bounds.size(),
            cosmic.corner_radii.radius_m.into(),
        );
        frame.fill(&preview, background);

        if self.style == TemperatureGaugeStyle::Text {
            draw_text_temperature_preview(&mut frame, bounds, track, active);
        } else {
            let radius = (bounds.height * 0.265).min(bounds.width * 0.13).min(27.0);
            let y = bounds.height / 2.0;
            draw_temperature_preview(
                &mut frame,
                self.style,
                Point::new(bounds.width * 0.3, y),
                radius,
                0.42,
                track,
                active,
            );
            draw_temperature_preview(
                &mut frame,
                self.style,
                Point::new(bounds.width * 0.7, y),
                radius,
                0.68,
                track,
                active,
            );
        }

        vec![frame.into_geometry()]
    }
}

fn draw_temperature_preview(
    frame: &mut canvas::Frame<cosmic::Renderer>,
    style: TemperatureGaugeStyle,
    center: Point,
    radius: f32,
    progress: f32,
    track: Color,
    active: Color,
) {
    const WIDTH: f32 = 5.0;
    let (start, sweep) = match style {
        TemperatureGaugeStyle::Arc => (3.0 * PI / 4.0, 3.0 * PI / 2.0),
        TemperatureGaugeStyle::Circular => (-PI / 2.0, 2.0 * PI),
        TemperatureGaugeStyle::Text => return,
    };

    if style == TemperatureGaugeStyle::Circular {
        frame.stroke(
            &canvas::Path::circle(center, radius),
            canvas::Stroke::default()
                .with_color(track)
                .with_width(WIDTH),
        );
    } else {
        frame.stroke(
            &preview_arc(center, radius, start, start + sweep),
            canvas::Stroke::default()
                .with_color(track)
                .with_width(WIDTH)
                .with_line_cap(canvas::LineCap::Round),
        );
    }

    frame.stroke(
        &preview_arc(center, radius, start, start + sweep * progress),
        canvas::Stroke::default()
            .with_color(active)
            .with_width(WIDTH)
            .with_line_cap(canvas::LineCap::Round),
    );
}

fn draw_text_temperature_preview(
    frame: &mut canvas::Frame<cosmic::Renderer>,
    bounds: Rectangle,
    track: Color,
    active: Color,
) {
    for (index, width) in [0.42_f32, 0.68].into_iter().enumerate() {
        let y = 31.0 + index as f32 * 41.0;
        frame.fill(&canvas::Path::circle(Point::new(22.0, y), 5.0), active);
        frame.fill(
            &canvas::Path::rounded_rectangle(
                Point::new(36.0, y - 4.0),
                Size::new((bounds.width - 98.0).max(24.0), 8.0),
                4.0.into(),
            ),
            track,
        );
        frame.fill(
            &canvas::Path::rounded_rectangle(
                Point::new(bounds.width - 49.0, y - 7.0),
                Size::new(33.0 * width + 12.0, 14.0),
                5.0.into(),
            ),
            active,
        );
    }
}

fn preview_arc(center: Point, radius: f32, start: f32, end: f32) -> canvas::Path {
    canvas::Path::new(|builder| {
        builder.arc(canvas::path::Arc {
            center,
            radius,
            start_angle: Radians(start),
            end_angle: Radians(end),
        });
    })
}
