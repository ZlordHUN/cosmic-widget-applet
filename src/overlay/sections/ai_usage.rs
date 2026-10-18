// SPDX-License-Identifier: MPL-2.0

use crate::monitors::ai_usage::{
    CreditBalance, UsageSnapshot, UsageSource, UsageStatus, UsageWindow,
};
use crate::overlay::Message;
use crate::overlay::components::gauge;
use crate::overlay::components::section::{CARD_SPACING, CARD_VERTICAL_PADDING};
use chrono::{DateTime, Local};
use cosmic::iced::{Alignment, Background, Border, Color, ContentFit, Length};
use cosmic::{Element, theme, widget};

const HEADING_HEIGHT: f32 = 24.0;
const SECTION_SPACING: u16 = 10;
const CARD_GAP: u16 = 8;
const CARD_PADDING: u16 = 10;
const PROVIDER_SPACING: u16 = 12;
const WINDOW_HEIGHT: f32 = 56.0;
const WINDOW_SPACING: u16 = 14;
const CREDITS_HEIGHT: f32 = HEADING_HEIGHT + 2.0 * CARD_PADDING as f32;

fn card_body_height(codex: &UsageSnapshot, claude: &UsageSnapshot) -> f32 {
    // Keep the reference's two-row cards stable while reports load. Additional
    // model-specific buckets stay visible inside their own provider's card.
    let rows = codex.windows.len().max(claude.windows.len()).max(2);
    rows as f32 * WINDOW_HEIGHT + (rows - 1) as f32 * f32::from(WINDOW_SPACING)
}

pub(in crate::overlay) fn section_height(codex: &UsageSnapshot, claude: &UsageSnapshot) -> f32 {
    HEADING_HEIGHT
        + f32::from(SECTION_SPACING)
        + 2.0 * f32::from(CARD_PADDING)
        + HEADING_HEIGHT
        + f32::from(PROVIDER_SPACING)
        + card_body_height(codex, claude)
        + f32::from(SECTION_SPACING)
        + CREDITS_HEIGHT
        + 2.0 * f32::from(CARD_VERTICAL_PADDING)
        + f32::from(CARD_SPACING)
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

    fn update_hint(self, desktop_report: bool) -> &'static str {
        match self {
            Self::Codex => "Updates from local Codex responses every 30 seconds.",
            Self::Claude if desktop_report => {
                "Checks saved Claude Desktop usage every 30 seconds. Desktop normally saves reports every 5–15 minutes. Reports older than 20 minutes are marked stale."
            }
            Self::Claude => "Checks your Claude account's remaining usage every 5 minutes.",
        }
    }

    fn unavailable_message(self, status: UsageStatus) -> &'static str {
        match (self, status) {
            (Self::Codex, UsageStatus::Loading) => "Checking Codex usage…",
            (Self::Claude, UsageStatus::Loading) => "Checking Claude usage…",
            (Self::Codex, _) => "Usage appears after your next Codex response.",
            (Self::Claude, UsageStatus::SignInRequired) => {
                "Sign in to Claude Desktop or Claude Code to load usage."
            }
            (Self::Claude, _) => "Claude usage is unavailable. Retrying shortly…",
        }
    }
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
    let body_height = card_body_height(codex, claude);
    let cards = widget::row::with_capacity(2)
        .width(Length::Fill)
        .spacing(CARD_GAP)
        .push(provider_card(
            Provider::Codex,
            codex,
            now,
            use_24hour_time,
            body_height,
        ))
        .push(provider_card(
            Provider::Claude,
            claude,
            now,
            use_24hour_time,
            body_height,
        ));
    widget::column::with_capacity(3)
        .spacing(SECTION_SPACING)
        .push(heading)
        .push(cards)
        .push(credits_row(codex, now))
        .into()
}

fn provider_card<'a>(
    provider: Provider,
    usage: &'a UsageSnapshot,
    now: DateTime<Local>,
    use_24hour_time: bool,
    body_height: f32,
) -> Element<'a, Message> {
    let body = if usage.windows.is_empty() {
        widget::container(unavailable_usage(provider, usage, now))
            .width(Length::Fill)
            .center_y(Length::Fixed(body_height))
    } else {
        let mut windows = widget::column::with_capacity(usage.windows.len())
            .width(Length::Fill)
            .spacing(WINDOW_SPACING);
        for window in &usage.windows {
            windows = windows.push(usage_window(provider, usage, window, now, use_24hour_time));
        }
        widget::container(windows)
            .width(Length::Fill)
            .height(body_height)
    };
    widget::container(
        widget::column::with_capacity(2)
            .width(Length::Fill)
            .spacing(PROVIDER_SPACING)
            .push(provider_heading(provider, usage, now))
            .push(body),
    )
    .width(Length::FillPortion(1))
    .padding(CARD_PADDING)
    .class(theme::Container::custom(provider_surface))
    .into()
}

fn provider_surface(theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    let foreground: Color = theme.current_container().on.into();
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(Color {
            a: 0.035,
            ..foreground
        })),
        border: Border {
            color: Color {
                a: 0.12,
                ..foreground
            },
            width: 1.0,
            radius: 16.0.into(),
        },
        ..Default::default()
    }
}

struct CreditDetails {
    value: String,
    stale: bool,
    tooltip: String,
}

fn credit_details(usage: &UsageSnapshot, now: DateTime<Local>) -> CreditDetails {
    let Some(credits) = &usage.credits else {
        return CreditDetails {
            value: if usage.status == UsageStatus::Loading { "Loading…" } else { "—" }.into(),
            stale: false,
            tooltip: "ChatGPT credit balance unavailable.\nAppears when Codex saves an account usage report that includes credits.".into(),
        };
    };
    let (value, balance) = match &credits.balance {
        CreditBalance::Amount(amount) => {
            // Strip decimal padding without rounding away any reported credit.
            let display = if amount.contains('.') {
                amount.trim_end_matches('0').trim_end_matches('.')
            } else {
                amount.as_str()
            };
            (
                format!("{display} left"),
                format!("{amount} credits remaining"),
            )
        }
        CreditBalance::Unlimited => ("Unlimited".into(), "Unlimited credits".into()),
    };
    CreditDetails {
        value,
        stale: credits.is_stale,
        tooltip: format!(
            "ChatGPT credits · {}\n{balance}\nShared by Codex and ChatGPT Work.\n{}\nUpdates from local Codex reports every 30 seconds.",
            if credits.is_stale {
                "Last known balance"
            } else {
                "Reported balance"
            },
            report_age(Some(credits.updated_at), now.timestamp()),
        ),
    }
}

fn credits_row(usage: &UsageSnapshot, now: DateTime<Local>) -> Element<'static, Message> {
    let details = credit_details(usage, now);
    let mut icon = widget::icon::from_svg_bytes(Provider::Codex.icon());
    icon.symbolic = true;
    let mut row = widget::row::with_capacity(4)
        .width(Length::Fill)
        .height(HEADING_HEIGHT)
        .align_y(Alignment::Center)
        .spacing(8)
        .push(
            widget::icon::icon(icon)
                .size(18)
                .content_fit(ContentFit::Contain),
        )
        .push(widget::text::caption_heading("ChatGPT credits").size(12));
    if details.stale {
        row = row.push(widget::text::caption("Stale").size(10));
    }
    row = row.push(
        widget::text::body(details.value)
            .size(13)
            .width(Length::Fill)
            .align_x(cosmic::iced::alignment::Horizontal::Right)
            .wrapping(cosmic::iced::widget::text::Wrapping::None)
            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
            )),
    );
    widget::tooltip(
        widget::container(row)
            .width(Length::Fill)
            .height(CREDITS_HEIGHT)
            .padding(CARD_PADDING)
            .class(theme::Container::custom(provider_surface)),
        widget::text::caption(details.tooltip),
        widget::tooltip::Position::Top,
    )
    .into()
}

fn provider_heading(
    provider: Provider,
    usage: &UsageSnapshot,
    now: DateTime<Local>,
) -> Element<'static, Message> {
    let mut icon = widget::icon::from_svg_bytes(provider.icon());
    icon.symbolic = true;
    let mut heading = widget::row::with_capacity(4)
        .width(Length::Fill)
        .height(HEADING_HEIGHT)
        .align_y(Alignment::Center)
        .spacing(6)
        .push(
            widget::icon::icon(icon)
                .size(18)
                .content_fit(ContentFit::Contain),
        )
        .push(widget::text::heading(provider.name()).size(13))
        .push(widget::space::horizontal());
    let stale = usage.status == UsageStatus::Stale
        || usage
            .windows
            .iter()
            .any(|window| window.is_stale || reset_time(window).is_some_and(|reset| reset <= now));
    let badge = if stale {
        Some("Stale")
    } else {
        match usage.status {
            UsageStatus::Loading => Some("Loading"),
            UsageStatus::SignInRequired => Some("Sign in"),
            UsageStatus::Unavailable => Some("No data"),
            _ => None,
        }
    };
    if let Some(badge) = badge {
        heading = heading.push(
            widget::container(widget::text::caption(badge).size(10))
                .padding([4, 6])
                .class(theme::Container::custom(|theme| {
                    let foreground: Color = theme.current_container().on.into();
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(Color {
                            a: 0.10,
                            ..foreground
                        })),
                        border: Border {
                            color: Color {
                                a: 0.08,
                                ..foreground
                            },
                            width: 1.0,
                            radius: 11.0.into(),
                        },
                        ..Default::default()
                    }
                })),
        );
    }
    heading.into()
}

fn detail_row(left: String, right: String) -> Element<'static, Message> {
    widget::row::with_capacity(2)
        .width(Length::Fill)
        .height(16)
        .spacing(3)
        .push(widget::text::caption(left).size(10).width(Length::Fill))
        .push(widget::text::caption(right).size(10))
        .into()
}

struct UsageWindowDetails {
    remaining_percent: Option<f32>,
    stale: bool,
    reset_caption: String,
    tooltip: String,
}

fn usage_window_details(
    provider: Provider,
    usage: &UsageSnapshot,
    window: &UsageWindow,
    now: DateTime<Local>,
    use_24hour_time: bool,
) -> UsageWindowDetails {
    let expired = reset_time(window).is_some_and(|reset| reset <= now);
    // Sources timestamp each allowance independently. An older model bucket
    // must not mark a freshly refreshed account allowance as stale.
    let stale = expired || window.is_stale;
    let value = (!expired && window.remaining_percent.is_finite())
        .then(|| window.remaining_percent.clamp(0.0, 100.0));
    let desktop_report = usage.source == Some(UsageSource::ClaudeDesktop);
    let state = if expired {
        "Awaiting a new report"
    } else if stale {
        "Last known usage"
    } else if desktop_report {
        "Saved Claude Desktop report"
    } else {
        "Remaining usage"
    };
    let tooltip = format!(
        "{} · {}\n{}\n{}\n{} · {}\n{}",
        provider.name(),
        window.label,
        state,
        reset_label(window, now, use_24hour_time, provider.name()),
        if desktop_report {
            format!(
                "Reported {}",
                compact_report_age(window.updated_at, now.timestamp())
            )
        } else {
            report_age(Some(window.updated_at), now.timestamp())
        },
        value.map_or_else(|| "—".into(), |value| format!("{value:.0}% left")),
        provider.update_hint(desktop_report),
    );
    UsageWindowDetails {
        remaining_percent: value,
        stale,
        reset_caption: compact_reset_label(window, now, use_24hour_time),
        tooltip,
    }
}

fn usage_window<'a>(
    provider: Provider,
    usage: &'a UsageSnapshot,
    window: &'a UsageWindow,
    now: DateTime<Local>,
    use_24hour_time: bool,
) -> Element<'a, Message> {
    let details = usage_window_details(provider, usage, window, now, use_24hour_time);
    let value = details.remaining_percent;
    let mut title = widget::row::with_capacity(2)
        .width(Length::Fill)
        .height(20)
        .align_y(Alignment::Center)
        .spacing(4);
    // The primary session allowance leads with its value, as in the reference.
    // Weekly and additional model-specific limits retain their own labels.
    if window.label != "5-hour" {
        title = title.push(
            widget::text::caption_heading(&window.label)
                .size(12)
                .width(Length::Fill)
                .wrapping(cosmic::iced::widget::text::Wrapping::None)
                .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                    cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                )),
        );
    }
    title = title.push(
        widget::text::body(
            value.map_or_else(|| "— left".into(), |value| format!("{value:.0}% left")),
        )
        .size(12),
    );
    let (left, right) = details
        .reset_caption
        .split_once('\n')
        .unwrap_or((&details.reset_caption, ""));
    let content = widget::column::with_capacity(5)
        .width(Length::Fill)
        .height(WINDOW_HEIGHT)
        .push(title)
        .push(widget::space().height(4))
        .push(gauge::remaining_bar(value, details.stale))
        .push(widget::space().height(8))
        .push(detail_row(left.into(), right.into()));
    widget::tooltip(
        content,
        widget::text::caption(details.tooltip),
        widget::tooltip::Position::Top,
    )
    .into()
}

fn unavailable_usage(
    provider: Provider,
    usage: &UsageSnapshot,
    now: DateTime<Local>,
) -> Element<'static, Message> {
    let updated = usage.updated_at.map_or_else(
        || "Last update unknown".into(),
        |updated| report_age(Some(updated), now.timestamp()),
    );
    let value = widget::row::with_capacity(2)
        .height(28)
        .align_y(Alignment::Center)
        .spacing(8)
        .push(widget::text::title4("—"))
        .push(widget::text::caption("left").size(12));
    let content = widget::column::with_capacity(5)
        .width(Length::Fill)
        .push(value)
        .push(widget::space().height(12))
        .push(gauge::remaining_bar(None, true))
        .push(widget::space().height(10))
        .push(widget::text::caption(updated).size(10));
    widget::tooltip(
        content,
        widget::text::caption(provider.unavailable_message(usage.status)),
        widget::tooltip::Position::Top,
    )
    .into()
}

fn reset_time(window: &UsageWindow) -> Option<DateTime<Local>> {
    window
        .resets_at
        .filter(|timestamp| *timestamp > 0)
        .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0))
        .map(|reset| reset.with_timezone(&Local))
}

fn compact_reset_label(
    window: &UsageWindow,
    now: DateTime<Local>,
    use_24hour_time: bool,
) -> String {
    let Some(reset) = reset_time(window) else {
        return "Reset time\nunavailable".to_string();
    };
    if reset.timestamp() <= now.timestamp() {
        return "Awaiting\nupdate".to_string();
    }
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
    let Some(reset) = reset_time(window) else {
        return "Reset time unavailable".to_string();
    };
    if reset.timestamp() <= now.timestamp() {
        return format!("Reset passed · Awaiting {provider} update");
    }
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
    format!("Updated {}", compact_report_age(updated_at, now))
}

fn compact_report_age(updated_at: i64, now: i64) -> String {
    let seconds = now.saturating_sub(updated_at).max(0);
    match seconds {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render the actual section without opening a window or starting monitors.
    #[tokio::test]
    #[ignore = "writes deterministic AI Usage previews to /tmp"]
    async fn render_ai_usage_preview() {
        use chrono::TimeZone;
        use cosmic::iced::advanced::renderer::Headless as _;
        use cosmic::iced::advanced::widget::Tree;
        use cosmic::iced::advanced::{Layout, Renderer as _, layout, renderer};
        use cosmic::iced::{Font, Pixels, Rectangle, Size, mouse};
        use cosmic::{Renderer, Theme};

        let now = Local.with_ymd_and_hms(2026, 10, 15, 0, 53, 5).unwrap();
        let window = |label: &str, remaining_percent: f32, reset: Option<i64>| UsageWindow {
            limit_id: label.into(),
            label: label.into(),
            remaining_percent,
            resets_at: reset,
            updated_at: now.timestamp() - 300,
            is_stale: false,
        };
        let mut codex = UsageSnapshot {
            status: UsageStatus::Stale,
            ..Default::default()
        };
        let mut claude = UsageSnapshot {
            windows: vec![
                window("5-hour", 97.0, Some(now.timestamp() + 3 * 3600 + 36 * 60)),
                window(
                    "Weekly",
                    66.0,
                    Some(now.timestamp() + 5 * 86400 + 19 * 3600 + 6 * 60),
                ),
            ],
            updated_at: Some(now.timestamp() - 300),
            status: UsageStatus::Available,
            source: Some(UsageSource::ClaudeCode),
            credits: None,
        };
        let mut renderer =
            <Renderer as renderer::Headless>::new(Font::default(), Pixels(14.0), Some("tiny-skia"))
                .await
                .expect("tiny-skia software renderer");
        let preview_epoch = cosmic::iced::time::Instant::now();
        for case in [
            "reference",
            "available",
            "low",
            "expired",
            "desktop",
            "bars-seek",
            "bars-hover",
            "bars-readonly",
            "meters",
            "meters-circular",
            "meters-charging",
            "credits",
            "credits-zero",
            "credits-unlimited",
            "credits-stale",
            "credits-unavailable",
            "credits-long",
        ] {
            match case {
                "available" => {
                    codex.status = UsageStatus::Available;
                    codex.windows = vec![
                        window("5-hour", 100.0, Some(now.timestamp() + 3600)),
                        window("Weekly", 0.0, Some(now.timestamp() + 86400)),
                    ];
                }
                "low" => {
                    codex.windows[0].remaining_percent = 45.0;
                    codex.windows[1].remaining_percent = 15.0;
                }
                "expired" => {
                    codex.status = UsageStatus::Stale;
                    codex.windows[0].resets_at = Some(now.timestamp() - 60);
                    claude.windows.push(window(
                        "Extra model allowance",
                        28.0,
                        Some(now.timestamp() + 86400),
                    ));
                }
                "desktop" => {
                    codex.windows.clear();
                    codex.status = UsageStatus::SignInRequired;
                    claude.source = Some(UsageSource::ClaudeDesktop);
                    claude.windows.truncate(2);
                    for window in &mut claude.windows {
                        window.resets_at = None;
                    }
                }
                _ => {}
            }
            if case.starts_with("credits") {
                codex = UsageSnapshot {
                    windows: vec![window("Weekly", 0.0, Some(now.timestamp() + 86400))],
                    status: UsageStatus::Available,
                    credits: if case == "credits-unavailable" {
                        None
                    } else {
                        Some(crate::monitors::ai_usage::UsageCredits {
                            balance: match case {
                                "credits-unlimited" => CreditBalance::Unlimited,
                                "credits-zero" => CreditBalance::Amount("0".into()),
                                "credits-long" => CreditBalance::Amount(
                                    "123456789012345678901234567890.123456789".into(),
                                ),
                                _ => CreditBalance::Amount("12345.6789010000".into()),
                            },
                            updated_at: now.timestamp()
                                - if case == "credits-stale" { 1800 } else { 300 },
                            is_stale: case == "credits-stale",
                        })
                    },
                    ..Default::default()
                };
                claude.source = Some(UsageSource::ClaudeCode);
                for window in &mut claude.windows {
                    window.resets_at = Some(now.timestamp() + 3600);
                }
            }
            let theme = if case.starts_with("bars-") || case.starts_with("meters") {
                cosmic::theme::system_preference()
            } else {
                Theme::dark()
            };
            let spacing = cosmic::theme::system_preference().cosmic().spacing;
            let outer = spacing.space_m / 2;
            let inner = spacing.space_m - outer;
            let mut media_stats = crate::overlay::stats::SystemSnapshot::default();
            media_stats.media.players.push((
                crate::monitors::media::PlayerId::Mpris("org.mpris.MediaPlayer2.preview".into()),
                crate::monitors::media::MediaInfo {
                    player_name: "Preview".into(),
                    title: "Shared bar style".into(),
                    artist: "Sample data".into(),
                    status: crate::monitors::media::PlaybackStatus::Playing,
                    position: 50_000,
                    duration: 100_000,
                    can_seek: case != "bars-readonly",
                    can_play: true,
                    can_pause: true,
                    ..Default::default()
                },
            ));
            let meter_config = crate::config::Config {
                show_cpu_temp: true,
                show_gpu_temp: true,
                temperature_gauge_style: if case == "meters-circular" {
                    crate::config::TemperatureGaugeStyle::Circular
                } else {
                    crate::config::TemperatureGaugeStyle::Arc
                },
                ..Default::default()
            };
            let mut meter_stats = crate::overlay::stats::SystemSnapshot {
                cpu_temp: 36.0,
                gpu_temp: 64.0,
                ..Default::default()
            };
            let hot_stats = crate::overlay::stats::SystemSnapshot {
                cpu_temp: 86.0,
                gpu_temp: 100.0,
                ..Default::default()
            };
            for (name, kind, level, charging) in [
                ("Mouse · static", "mouse", 75, false),
                ("Keyboard · static", "keyboard", 25, false),
                ("Headset · static", "headset", 8, false),
                ("Controller · charging", "controller", 80, true),
            ] {
                meter_stats
                    .devices
                    .push(crate::monitors::battery::BatteryDevice {
                        name: name.into(),
                        kind: Some(kind.into()),
                        level: Some(level),
                        status: Some(if charging { "charging" } else { "discharging" }.into()),
                        codename: None,
                        is_loading: false,
                        is_connected: true,
                    });
            }
            let mut meter_devices = crate::overlay::device_state::DeviceList::default();
            meter_devices.reconcile(
                &meter_stats.devices,
                std::time::Instant::now(),
                crate::overlay::sections::devices::reveal_row_height(12),
            );
            let content: Element<'_, Message> = if case.starts_with("meters") {
                widget::column::with_capacity(5)
                    .spacing(12)
                    .push(widget::text::caption("Normal / warning temperature bands"))
                    .push(crate::overlay::sections::system::temperature_view(
                        &meter_config,
                        &meter_stats,
                        12,
                        8,
                    ))
                    .push(widget::text::caption("Danger / full temperature bands"))
                    .push(crate::overlay::sections::system::temperature_view(
                        &meter_config,
                        &hot_stats,
                        12,
                        8,
                    ))
                    .push(crate::overlay::sections::devices::devices_view(
                        &meter_devices,
                        12,
                        8,
                    ))
                    .into()
            } else if case.starts_with("bars-") {
                codex = UsageSnapshot {
                    windows: vec![
                        window("5-hour", 45.0, Some(now.timestamp() + 3600)),
                        window("Weekly", 15.0, Some(now.timestamp() + 86400)),
                    ],
                    status: UsageStatus::Available,
                    ..Default::default()
                };
                let mut gallery = widget::column::with_capacity(12).spacing(12);
                for (label, value) in [
                    ("Utilization · 9%", 9.0),
                    ("Storage · 65%", 65.0),
                    ("Storage · 86%", 86.0),
                ] {
                    gallery = gallery
                        .push(widget::text::caption(label))
                        .push(gauge::indicator_bar(value));
                }
                gallery = gallery
                    .push(widget::text::caption("File transfer · 85%"))
                    .push(gauge::progress_bar(85.0))
                    .push(crate::overlay::sections::media::media_view(
                        &media_stats,
                        None,
                        None,
                        8,
                        8,
                        4,
                        case == "bars-hover",
                    ))
                    .push(view(&codex, &claude, now, false));
                gallery.into()
            } else {
                view(&codex, &claude, now, false)
            };
            let mut element: Element<'_, Message> = widget::container(
                crate::overlay::components::section::section_card(content, inner),
            )
            .padding([16, outer])
            .width(370.0)
            .into();
            let mut tree = Tree::new(element.as_widget());
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(370.0, 4096.0)),
            );
            let height = node.size().height.ceil() as u32;
            assert!(height > 32 && height < 4096);
            if case.starts_with("credits") {
                // Preview adds 32px outer padding; section_height includes an
                // 8px inter-section gap that is not part of the rendered card.
                let expected = section_height(&codex, &claude) - f32::from(CARD_SPACING) + 32.0;
                assert!(
                    (node.size().height - expected).abs() < 1.0,
                    "{case}: rendered {}, reserved {expected}",
                    node.size().height
                );
            }
            let viewport = Rectangle::with_size(Size::new(370.0, height as f32));
            renderer.reset(viewport);
            let foreground: Color = theme.current_container().on.into();
            let style = renderer::Style {
                text_color: foreground,
                icon_color: foreground,
                ..Default::default()
            };
            let mut messages = Vec::new();
            element.as_widget_mut().update(
                &mut tree,
                &cosmic::iced::Event::Window(cosmic::iced::window::Event::RedrawRequested(
                    preview_epoch,
                )),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut cosmic::iced::advanced::clipboard::Null,
                &mut cosmic::iced::advanced::Shell::new(&mut messages),
                &viewport,
            );
            if case == "meters-charging" {
                // Advance the real charging widget halfway through its sweep.
                // Synthetic redraw time avoids sleeping or changing its private state.
                element.as_widget_mut().update(
                    &mut tree,
                    &cosmic::iced::Event::Window(cosmic::iced::window::Event::RedrawRequested(
                        preview_epoch + std::time::Duration::from_millis(650),
                    )),
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &renderer,
                    &mut cosmic::iced::advanced::clipboard::Null,
                    &mut cosmic::iced::advanced::Shell::new(&mut messages),
                    &viewport,
                );
            }
            element.as_widget().draw(
                &tree,
                &mut renderer,
                &theme,
                &style,
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            let pixels = renderer.screenshot(
                Size::new(370, height),
                1.0,
                theme.cosmic().background(false).base.into(),
            );
            let path = format!("/tmp/cosmic-ai-usage-{case}.png");
            image::save_buffer(&path, &pixels, 370, height, image::ColorType::Rgba8).unwrap();
            println!("AI_USAGE_PREVIEW {path} 370x{height}");
        }
    }

    #[test]
    fn credits_preserve_exact_balances_and_distinguish_unavailable_and_unlimited() {
        let now = DateTime::from_timestamp(2_000_000_000, 0)
            .unwrap()
            .with_timezone(&Local);
        let mut usage = UsageSnapshot::default();
        assert_eq!(credit_details(&usage, now).value, "—");
        for amount in ["0", "0.0001", "12345678901234567890.123456789"] {
            usage.credits = Some(crate::monitors::ai_usage::UsageCredits {
                balance: CreditBalance::Amount(amount.into()),
                updated_at: now.timestamp() - 300,
                is_stale: false,
            });
            let details = credit_details(&usage, now);
            assert_eq!(details.value, format!("{amount} left"));
            assert!(details.tooltip.contains(amount));
            assert!(!details.tooltip.contains("Resets"));
            assert!(!details.value.contains('%'));
        }
        usage.credits.as_mut().unwrap().balance = CreditBalance::Amount("12345.67890000".into());
        let details = credit_details(&usage, now);
        assert_eq!(details.value, "12345.6789 left");
        assert!(details.tooltip.contains("12345.67890000"));
        usage.credits.as_mut().unwrap().balance = CreditBalance::Amount("0.0000".into());
        assert_eq!(credit_details(&usage, now).value, "0 left");
        usage.credits.as_mut().unwrap().balance = CreditBalance::Unlimited;
        assert_eq!(credit_details(&usage, now).value, "Unlimited");
    }

    #[test]
    fn credit_freshness_is_independent_of_quota_status() {
        let now = DateTime::from_timestamp(2_000_000_000, 0)
            .unwrap()
            .with_timezone(&Local);
        let mut usage = UsageSnapshot {
            status: UsageStatus::Stale,
            credits: Some(crate::monitors::ai_usage::UsageCredits {
                balance: CreditBalance::Amount("12.5".into()),
                updated_at: now.timestamp() - 300,
                is_stale: false,
            }),
            ..Default::default()
        };
        assert!(!credit_details(&usage, now).stale);
        usage.status = UsageStatus::Available;
        let credits = usage.credits.as_mut().unwrap();
        credits.updated_at = now.timestamp() - 1800;
        credits.is_stale = true;
        let details = credit_details(&usage, now);
        assert!(details.stale);
        assert!(details.tooltip.contains("Last known balance"));
        assert!(details.tooltip.contains("Updated 30m ago"));
    }

    #[test]
    fn report_age_never_claims_a_missing_snapshot_is_current() {
        assert_eq!(report_age(None, 1000), "Waiting for usage report");
        assert_eq!(report_age(Some(1000), 1001), "Updated just now");
        assert_eq!(report_age(Some(1000), 1900), "Updated 15m ago");
        assert_eq!(report_age(Some(1000), 87400), "Updated 1d ago");
    }

    fn desktop_window() -> (UsageSnapshot, UsageWindow, DateTime<Local>) {
        use chrono::TimeZone;

        let now = Local.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).unwrap();
        let usage = UsageSnapshot {
            status: UsageStatus::Available,
            source: Some(UsageSource::ClaudeDesktop),
            ..Default::default()
        };
        let window = UsageWindow {
            limit_id: "five_hour".into(),
            label: "5-hour".into(),
            remaining_percent: 70.0,
            resets_at: None,
            updated_at: now.timestamp() - 900,
            is_stale: false,
        };
        (usage, window, now)
    }

    #[test]
    fn fresh_codex_allowance_is_not_stale_when_an_unrelated_model_bucket_is_old() {
        let now = DateTime::from_timestamp(2_000_000_000, 0)
            .unwrap()
            .with_timezone(&Local);
        let fresh = UsageWindow {
            limit_id: "codex".into(),
            label: "Weekly".into(),
            remaining_percent: 98.0,
            resets_at: Some(now.timestamp() + 604800),
            updated_at: now.timestamp(),
            is_stale: false,
        };
        let old = UsageWindow {
            limit_id: "codex_model_specific".into(),
            label: "Other model · Weekly".into(),
            updated_at: now.timestamp() - 1800,
            is_stale: true,
            ..fresh.clone()
        };
        let usage = UsageSnapshot {
            windows: vec![fresh, old],
            status: UsageStatus::Stale,
            updated_at: Some(now.timestamp()),
            ..Default::default()
        };
        let fresh = usage_window_details(Provider::Codex, &usage, &usage.windows[0], now, true);
        assert!(!fresh.stale);
        assert_eq!(fresh.remaining_percent, Some(98.0));
        assert!(fresh.tooltip.contains("Remaining usage"));
        assert!(!fresh.tooltip.contains("Last known usage"));

        let old = usage_window_details(Provider::Codex, &usage, &usage.windows[1], now, true);
        assert!(old.stale);
        assert!(old.tooltip.contains("Last known usage"));
    }

    #[test]
    fn claude_desktop_reports_keep_their_individual_stale_flag() {
        let (usage, mut window, now) = desktop_window();
        window.is_stale = true;
        window.resets_at = Some(now.timestamp() + 3600);
        let details = usage_window_details(Provider::Claude, &usage, &window, now, true);
        assert!(details.stale);
        assert_eq!(details.remaining_percent, Some(70.0));
        assert!(details.tooltip.contains("Last known usage"));
        assert!(details.tooltip.contains("Reported 15m ago"));
    }

    #[test]
    fn desktop_source_displays_real_reset_times_in_both_clock_formats() {
        let (usage, mut window, now) = desktop_window();
        window.resets_at = Some(now.timestamp() + 3600);

        let details = usage_window_details(Provider::Claude, &usage, &window, now, true);
        assert_eq!(details.reset_caption, "Resets today\n13:00");
        assert!(details.tooltip.contains("Resets today at 13:00"));
        assert!(details.tooltip.contains("Saved Claude Desktop report"));
        assert!(details.tooltip.contains("Reported 15m ago"));
        assert!(!details.tooltip.contains("do not include reset times"));
        assert_eq!(details.remaining_percent, Some(70.0));
        assert!(!details.stale);

        let details = usage_window_details(Provider::Claude, &usage, &window, now, false);
        assert_eq!(details.reset_caption, "Resets today\n1:00 PM");
        assert!(details.tooltip.contains("Resets today at 1:00 PM"));
    }

    #[test]
    fn missing_or_invalid_resets_keep_report_age_without_inventing_a_timer() {
        let (usage, mut window, now) = desktop_window();
        for reset in [None, Some(0), Some(-1), Some(i64::MAX)] {
            window.resets_at = reset;
            let details = usage_window_details(Provider::Claude, &usage, &window, now, true);
            assert_eq!(details.reset_caption, "Reset time\nunavailable");
            assert!(details.tooltip.contains("Reset time unavailable"));
            assert!(details.tooltip.contains("Reported 15m ago"));
            assert_eq!(details.remaining_percent, Some(70.0));
        }
        let later = now + chrono::Duration::minutes(45);
        let details = usage_window_details(Provider::Claude, &usage, &window, later, true);
        assert!(details.tooltip.contains("Reported 1h ago"));
        assert_eq!(compact_report_age(1_000, 999), "just now");
    }

    #[test]
    fn expired_desktop_reset_waits_without_refilling_allowance() {
        let (usage, mut window, now) = desktop_window();
        window.resets_at = Some(now.timestamp());
        let details = usage_window_details(Provider::Claude, &usage, &window, now, true);
        assert_eq!(details.reset_caption, "Awaiting\nupdate");
        assert!(
            details
                .tooltip
                .contains("Reset passed · Awaiting Claude update")
        );
        assert!(details.tooltip.contains("Awaiting a new report"));
        assert!(details.tooltip.contains("Reported 15m ago"));
        assert_eq!(details.remaining_percent, None);
        assert!(details.stale);
        assert_eq!(window.remaining_percent, 70.0);
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
