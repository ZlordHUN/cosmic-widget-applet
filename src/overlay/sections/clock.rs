// SPDX-License-Identifier: MPL-2.0

use crate::config::Config;
use crate::overlay::Message;
use chrono::{DateTime, Local};
use cosmic::iced::Alignment;
use cosmic::{Element, widget};

pub(in crate::overlay) fn clock_view<'a>(
    config: &Config,
    now: DateTime<Local>,
    spacing: u16,
) -> Element<'a, Message> {
    let mut clock = widget::column::with_capacity(2).spacing(spacing);

    if config.show_clock {
        let (time, suffix) = format_time_parts(now, config.use_24hour_time);
        let time_row = widget::row::with_capacity(2)
            .align_y(Alignment::Center)
            .spacing(spacing)
            .push(
                widget::text::text(time)
                    .size(48)
                    .font(cosmic::font::semibold()),
            )
            .push(widget::text::title4(suffix));
        clock = clock.push(time_row);
    }

    if config.show_date {
        let date_row = widget::row::with_capacity(2)
            .align_y(Alignment::Center)
            .spacing(spacing)
            .push(widget::icon::from_name("x-office-calendar-symbolic").size(16))
            .push(widget::text::body(now.format("%A, %-d %B %Y").to_string()));
        clock = clock.push(date_row);
    }

    clock.into()
}

fn format_time_parts(now: DateTime<Local>, use_24hour_time: bool) -> (String, String) {
    if use_24hour_time {
        (
            now.format("%H:%M").to_string(),
            now.format(":%S").to_string(),
        )
    } else {
        (
            now.format("%-I:%M").to_string(),
            now.format(":%S %p").to_string(),
        )
    }
}
