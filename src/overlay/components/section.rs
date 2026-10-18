// SPDX-License-Identifier: MPL-2.0

use crate::overlay::Message;
use cosmic::iced::{Alignment, Background, Border, Color, Length};
use cosmic::{Element, theme, widget};

pub(in crate::overlay) const METRIC_ICON_SIZE: u16 = 18;

pub(in crate::overlay) const CARD_VERTICAL_PADDING: u16 = 12;
pub(in crate::overlay) const CARD_SPACING: u16 = 8;

pub(in crate::overlay) fn section_card<'a>(
    content: Element<'a, Message>,
    horizontal_padding: u16,
) -> Element<'a, Message> {
    widget::container(content)
        .width(Length::Fill)
        .padding([CARD_VERTICAL_PADDING, horizontal_padding])
        .class(theme::Container::custom(|theme| {
            section_card_style(theme, 16.0)
        }))
        .into()
}

pub(in crate::overlay) fn section_card_style(
    theme: &cosmic::Theme,
    radius: f32,
) -> cosmic::iced::widget::container::Style {
    let foreground: Color = theme.current_container().on.into();
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(Color {
            a: 0.045,
            ..foreground
        })),
        border: Border {
            color: Color {
                a: 0.10,
                ..foreground
            },
            width: 1.0,
            radius: radius.into(),
        },
        ..Default::default()
    }
}

pub(in crate::overlay) fn section<'a>(
    icon_name: &'static str,
    title: &'a str,
    spacing: u16,
) -> cosmic::widget::Column<'a, Message, cosmic::Theme> {
    section_with_icon(
        widget::icon::from_name(icon_name).size(18).into(),
        title,
        spacing,
    )
}

pub(in crate::overlay) fn section_with_icon<'a>(
    icon: Element<'static, Message>,
    title: &'a str,
    spacing: u16,
) -> cosmic::widget::Column<'a, Message, cosmic::Theme> {
    let heading = widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(8)
        .push(icon)
        .push(widget::text::heading(title));

    widget::column::with_capacity(4)
        .spacing(spacing)
        .push(heading)
}

pub(in crate::overlay) fn embedded_symbolic_icon(
    bytes: &'static [u8],
    size: u16,
) -> Element<'static, Message> {
    let mut handle = widget::icon::from_svg_bytes(bytes);
    handle.symbolic = true;
    widget::icon::icon(handle).size(size).into()
}

pub(in crate::overlay) fn compact_single_line(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }

    let mut compact = normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    compact.push('…');
    compact
}
