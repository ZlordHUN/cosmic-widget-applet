// SPDX-License-Identifier: MPL-2.0

use crate::monitors::ai_usage::{UsageSnapshot, UsageStatus, UsageWindow};
use crate::overlay::Message;
use crate::overlay::components::gauge;
use chrono::{DateTime, Local};
use cosmic::iced::{Alignment, ContentFit, Length};
use cosmic::{Element, widget};

const GAUGE_SIZE: f32 = 88.0;
const GAUGES_PER_ROW: usize = 3;
const HEADING_HEIGHT: f32 = 24.0;
const SECTION_SPACING: u16 = 12;
const PROVIDER_SPACING: u16 = 8;
const LABEL_HEIGHT: f32 = 18.0;
const RESET_HEIGHT: f32 = 34.0;
const GAUGE_TILE_HEIGHT: f32 = GAUGE_SIZE + 4.0 + LABEL_HEIGHT + 6.0 + RESET_HEIGHT;
const GAUGE_ROW_HEIGHT: f32 = HEADING_HEIGHT + PROVIDER_SPACING as f32 + GAUGE_TILE_HEIGHT;

pub(in crate::overlay) fn section_height(codex: &UsageSnapshot, claude: &UsageSnapshot) -> f32 {
    let tiles = codex.windows.len().max(1) + claude.windows.len().max(1);
    let rows = tiles.div_ceil(GAUGES_PER_ROW) as f32;
    // The view uses the same fixed row heights; include the outer divider/margins.
    HEADING_HEIGHT + rows * (f32::from(SECTION_SPACING) + GAUGE_ROW_HEIGHT) + 33.0
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Provider {
    Codex,
    Claude,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
        }
    }

    fn icon(self) -> &'static [u8] {
        match self {
            Self::Codex => include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/openai-symbolic.svg"
            )),
            Self::Claude => include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/claude-symbolic.svg"
            )),
        }
    }

    fn update_hint(self) -> &'static str {
        match self {
            Self::Codex => "Updates from local Codex responses every 30 seconds.",
            Self::Claude => "Checks your Claude account's remaining usage every 5 minutes.",
        }
    }

    fn unavailable_message(self, status: UsageStatus) -> &'static str {
        match (self, status) {
            (Self::Codex, UsageStatus::Loading) => "Checking Codex usage…",
            (Self::Claude, UsageStatus::Loading) => "Checking Claude usage…",
            (Self::Codex, _) => "Usage appears after your next Codex response.",
            (Self::Claude, UsageStatus::SignInRequired) => "Sign in to Claude Code to load usage.",
            (Self::Claude, _) => "Claude usage is unavailable. Retrying shortly…",
        }
    }
}

#[derive(Clone, Copy)]
struct UsageTile<'a> {
    provider: Provider,
    usage: &'a UsageSnapshot,
    window: Option<&'a UsageWindow>,
}

pub(in crate::overlay) fn view<'a>(
    codex: &'a UsageSnapshot,
    claude: &'a UsageSnapshot,
    now: DateTime<Local>,
    use_24hour_time: bool,
) -> Element<'a, Message> {
    let mut icon = widget::icon::from_svg_bytes(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/icons/ai-sparkles-symbolic.svg"
    )));
    icon.symbolic = true;
    let heading = widget::row::with_capacity(2)
        .height(HEADING_HEIGHT)
        .align_y(Alignment::Center)
        .spacing(8)
        .push(
            widget::icon::icon(icon)
                .size(18)
                .content_fit(ContentFit::Contain),
        )
        .push(widget::text::heading("AI Usage"));
    let mut tiles = Vec::new();
    for (provider, usage) in [(Provider::Codex, codex), (Provider::Claude, claude)] {
        if usage.windows.is_empty() {
            tiles.push(UsageTile {
                provider,
                usage,
                window: None,
            });
        } else {
            tiles.extend(usage.windows.iter().map(|window| UsageTile {
                provider,
                usage,
                window: Some(window),
            }));
        }
    }

    let mut content = widget::column::with_capacity(tiles.len().div_ceil(GAUGES_PER_ROW) + 1)
        .spacing(SECTION_SPACING)
        .push(heading);
    for tiles in tiles.chunks(GAUGES_PER_ROW) {
        let mut row = widget::row::with_capacity(2)
            .width(Length::Fill)
            .height(GAUGE_ROW_HEIGHT);
        let mut start = 0;
        while start < tiles.len() {
            let provider = tiles[start].provider;
            let count = tiles[start..]
                .iter()
                .take_while(|tile| tile.provider == provider)
                .count();
            let mut gauges = widget::row::with_capacity(count).width(Length::Fill);
            for tile in &tiles[start..start + count] {
                gauges = gauges.push(usage_tile(*tile, now, use_24hour_time));
            }
            row = row.push(
                widget::column::with_capacity(2)
                    .width(Length::FillPortion(count as u16))
                    .align_x(Alignment::Center)
                    .spacing(PROVIDER_SPACING)
                    .push(provider_heading(provider))
                    .push(gauges),
            );
            start += count;
        }
        content = content.push(row);
    }
    content.into()
}

fn provider_heading(provider: Provider) -> Element<'static, Message> {
    let mut icon = widget::icon::from_svg_bytes(provider.icon());
    icon.symbolic = true;
    widget::row::with_capacity(2)
        .height(HEADING_HEIGHT)
        .align_y(Alignment::Center)
        .spacing(6)
        .push(
            widget::icon::icon(icon)
                .size(16)
                .content_fit(ContentFit::Contain),
        )
        .push(widget::text::heading(provider.name()))
        .into()
}

fn usage_tile<'a>(
    tile: UsageTile<'a>,
    now: DateTime<Local>,
    use_24hour_time: bool,
) -> Element<'a, Message> {
    let UsageTile {
        provider,
        usage,
        window,
    } = tile;
    let expired = window.is_some_and(|window| {
        window
            .resets_at
            .is_some_and(|reset| reset <= now.timestamp())
    });
    let stale = usage.status == UsageStatus::Stale || window.is_some_and(|window| window.is_stale);
    let value = window
        .filter(|_| !expired)
        .map(|window| window.remaining_percent);
    let (label, reset, tooltip) = if let Some(window) = window {
        let state = if expired {
            "Awaiting a new report"
        } else if stale {
            "Last known usage"
        } else {
            "Remaining usage"
        };
        (
            window.label.as_str(),
            compact_reset_label(window, now, use_24hour_time),
            format!(
                "{} · {}\n{}\n{}\n{} · {}\n{}",
                provider.name(),
                window.label,
                state,
                reset_label(window, now, use_24hour_time, provider.name()),
                report_age(Some(window.updated_at), now.timestamp()),
                if expired {
                    "—".to_string()
                } else {
                    format!("{:.0}% left", window.remaining_percent)
                },
                provider.update_hint()
            ),
        )
    } else {
        let (label, detail) = match (provider, usage.status) {
            (_, UsageStatus::Loading) => ("Loading…", "Checking\nusage"),
            (Provider::Claude, UsageStatus::SignInRequired) => ("Sign in", "Claude Code"),
            (Provider::Codex, _) => ("Waiting", "Next Codex\nresponse"),
            _ => ("Unavailable", "Retrying\nshortly"),
        };
        (
            label,
            detail.to_string(),
            provider.unavailable_message(usage.status).to_string(),
        )
    };
    let label = widget::text::caption_heading(label)
        .width(Length::Fill)
        .height(LABEL_HEIGHT)
        .align_x(Alignment::Center)
        .wrapping(cosmic::iced::widget::text::Wrapping::None)
        .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
            cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
        ));
    let content = widget::column::with_capacity(5)
        .width(Length::Fill)
        .height(GAUGE_TILE_HEIGHT)
        .align_x(Alignment::Center)
        .push(gauge::remaining_gauge(value, GAUGE_SIZE, stale))
        .push(widget::space().height(4))
        .push(label)
        .push(widget::space().height(6))
        .push(
            widget::text::caption(reset)
                .height(RESET_HEIGHT)
                .line_height(cosmic::iced::Pixels(17.0))
                .align_x(Alignment::Center),
        );
    widget::tooltip(
        content,
        widget::text::caption(tooltip),
        widget::tooltip::Position::Top,
    )
    .into()
}

fn compact_reset_label(
    window: &UsageWindow,
    now: DateTime<Local>,
    use_24hour_time: bool,
) -> String {
    let Some(reset) = window
        .resets_at
        .and_then(|reset| DateTime::from_timestamp(reset, 0))
    else {
        return "Reset time\nunavailable".to_string();
    };
    if reset.timestamp() <= now.timestamp() {
        return "Awaiting\nupdate".to_string();
    }
    let reset = reset.with_timezone(&Local);
    let day = if reset.date_naive() == now.date_naive() {
        "today".to_string()
    } else {
        reset.format("%-d %b").to_string()
    };
    let time = reset.format(if use_24hour_time {
        "%H:%M"
    } else {
        "%-I:%M %p"
    });
    format!("Resets {day}\n{time}")
}

fn reset_label(
    window: &UsageWindow,
    now: DateTime<Local>,
    use_24hour_time: bool,
    provider: &str,
) -> String {
    let Some(reset) = window
        .resets_at
        .and_then(|reset| DateTime::from_timestamp(reset, 0))
    else {
        return "Reset time unavailable".to_string();
    };
    if reset.timestamp() <= now.timestamp() {
        return format!("Reset passed · Awaiting {provider} update");
    }
    let reset = reset.with_timezone(&Local);
    let format = match (reset.date_naive() == now.date_naive(), use_24hour_time) {
        (true, true) => "today at %H:%M",
        (true, false) => "today at %-I:%M %p",
        (false, true) => "%a %-d %b at %H:%M",
        (false, false) => "%a %-d %b at %-I:%M %p",
    };
    format!("Resets {}", reset.format(format))
}

fn report_age(updated_at: Option<i64>, now: i64) -> String {
    let Some(updated_at) = updated_at else {
        return "Waiting for usage report".to_string();
    };
    let seconds = now.saturating_sub(updated_at).max(0);
    match seconds {
        0..=59 => "Updated just now".to_string(),
        60..=3599 => format!("Updated {}m ago", seconds / 60),
        3600..=86399 => format!("Updated {}h ago", seconds / 3600),
        _ => format!("Updated {}d ago", seconds / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_age_never_claims_a_missing_snapshot_is_current() {
        assert_eq!(report_age(None, 1000), "Waiting for usage report");
        assert_eq!(report_age(Some(1000), 1001), "Updated just now");
        assert_eq!(report_age(Some(1000), 1900), "Updated 15m ago");
        assert_eq!(report_age(Some(1000), 87400), "Updated 1d ago");
    }

    #[test]
    fn elapsed_reset_waits_for_a_new_report() {
        let now = DateTime::from_timestamp(2_000_000_000, 0)
            .unwrap()
            .with_timezone(&Local);
        let mut window = UsageWindow {
            limit_id: "codex".into(),
            label: "Weekly".into(),
            remaining_percent: 17.0,
            resets_at: Some(now.timestamp()),
            updated_at: now.timestamp() - 120,
            is_stale: true,
        };
        assert_eq!(
            reset_label(&window, now, true, "Codex"),
            "Reset passed · Awaiting Codex update"
        );
        assert_eq!(
            reset_label(&window, now, true, "Claude"),
            "Reset passed · Awaiting Claude update"
        );
        window.resets_at = None;
        assert_eq!(
            reset_label(&window, now, true, "Codex"),
            "Reset time unavailable"
        );
    }
}
