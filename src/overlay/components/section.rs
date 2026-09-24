// SPDX-License-Identifier: MPL-2.0

use crate::overlay::Message;
use cosmic::iced::Alignment;
use cosmic::{Element, widget};

pub(in crate::overlay) const METRIC_ICON_SIZE: u16 = 18;

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
