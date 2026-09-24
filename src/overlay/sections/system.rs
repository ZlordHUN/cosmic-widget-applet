// SPDX-License-Identifier: MPL-2.0

use crate::config::Config;
use crate::overlay::Message;
use crate::overlay::components::{
    gauge,
    section::{METRIC_ICON_SIZE, embedded_symbolic_icon, section, section_with_icon},
};
use crate::overlay::stats::SystemSnapshot;
use cosmic::iced::{Alignment, Length};
use cosmic::{Element, widget};

const METRIC_LABEL_WIDTH: f32 = 56.0;

pub(in crate::overlay) fn utilization_view<'a>(
    config: &Config,
    stats: &SystemSnapshot,
    section_spacing: u16,
    metric_spacing: u16,
) -> Element<'a, Message> {
    let mut section = section(
        "utilities-system-monitor-symbolic",
        "Utilization",
        section_spacing,
    );

    if config.show_cpu {
        section = section.push(metric(
            MetricIcon::Cpu,
            "CPU",
            stats.cpu_usage,
            config.show_percentages,
            metric_spacing,
        ));
    }
    if config.show_memory {
        section = section.push(metric(
            MetricIcon::Memory,
            "Memory",
            stats.memory_usage,
            config.show_percentages,
            metric_spacing,
        ));
    }
    if config.show_gpu {
        section = section.push(metric(
            MetricIcon::Gpu,
            "GPU",
            stats.gpu_usage,
            config.show_percentages,
            metric_spacing,
        ));
    }

    section.into()
}

pub(in crate::overlay) fn network_view<'a>(
    stats: &SystemSnapshot,
    section_spacing: u16,
    row_spacing: u16,
) -> Element<'a, Message> {
    section(
        "network-transmit-receive-symbolic",
        "Network",
        section_spacing,
    )
    .push(network_rate_row(
        "network-receive-symbolic",
        "Download",
        stats.network_rx_rate,
        row_spacing,
    ))
    .push(network_rate_row(
        "network-transmit-symbolic",
        "Upload",
        stats.network_tx_rate,
        row_spacing,
    ))
    .into()
}

fn network_rate_row<'a>(
    icon_name: &'static str,
    label: &'static str,
    bytes_per_second: f64,
    spacing: u16,
) -> Element<'a, Message> {
    widget::row::with_capacity(4)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(widget::icon::from_name(icon_name).size(METRIC_ICON_SIZE))
        .push(widget::text::body(label))
        .push(widget::space::horizontal())
        .push(widget::text::monotext(format_network_rate(
            bytes_per_second,
        )))
        .into()
}

pub(in crate::overlay) fn disk_io_view<'a>(
    stats: &SystemSnapshot,
    section_spacing: u16,
    row_spacing: u16,
) -> Element<'a, Message> {
    section(
        "drive-harddisk-solidstate-symbolic",
        "Disk I/O",
        section_spacing,
    )
    .push(network_rate_row(
        "document-open-symbolic",
        "Read",
        stats.disk_read_rate,
        row_spacing,
    ))
    .push(network_rate_row(
        "document-save-symbolic",
        "Write",
        stats.disk_write_rate,
        row_spacing,
    ))
    .into()
}

pub(in crate::overlay) fn temperature_view<'a>(
    config: &Config,
    stats: &SystemSnapshot,
    section_spacing: u16,
    gauge_spacing: u16,
) -> Element<'a, Message> {
    if config.temperature_gauge_style == crate::config::TemperatureGaugeStyle::Text {
        let mut temperatures = section_with_icon(
            embedded_symbolic_icon(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/icons/temperature-filled-symbolic.svg"
                )),
                18,
            ),
            "Temperatures",
            section_spacing,
        );
        if config.show_cpu_temp {
            temperatures = temperatures.push(temperature_text_item(
                MetricIcon::Cpu,
                "CPU",
                stats.cpu_temp,
                gauge_spacing,
            ));
        }
        if config.show_gpu_temp {
            temperatures = temperatures.push(temperature_text_item(
                MetricIcon::Gpu,
                "GPU",
                stats.gpu_temp,
                gauge_spacing,
            ));
        }
        return temperatures.into();
    }

    let mut gauges = widget::row::with_capacity(2)
        .spacing(gauge_spacing)
        .align_y(Alignment::Center);

    if config.show_cpu_temp {
        gauges = gauges.push(temperature_item(
            "CPU",
            stats.cpu_temp,
            config.temperature_gauge_style,
        ));
    }
    if config.show_gpu_temp {
        gauges = gauges.push(temperature_item(
            "GPU",
            stats.gpu_temp,
            config.temperature_gauge_style,
        ));
    }

    section_with_icon(
        embedded_symbolic_icon(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/temperature-filled-symbolic.svg"
            )),
            18,
        ),
        "Temperatures",
        section_spacing,
    )
    .push(widget::container(gauges).center_x(Length::Fill))
    .into()
}

fn metric<'a>(
    icon: MetricIcon,
    label: &'a str,
    value: f32,
    show_percentage: bool,
    spacing: u16,
) -> Element<'a, Message> {
    let value = value.clamp(0.0, 100.0);
    let mut metric_row = widget::row::with_capacity(4)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(metric_icon(icon))
        .push(
            widget::container(widget::text::caption_heading(label))
                .width(Length::Fixed(METRIC_LABEL_WIDTH)),
        )
        .push(gauge::indicator_bar(value));

    if show_percentage {
        metric_row = metric_row.push(widget::text::monotext(format!("{value:>5.1}%")));
    }

    metric_row.into()
}

fn format_network_rate(bytes_per_second: f64) -> String {
    const KB: f64 = 1_024.0;
    const MB: f64 = KB * 1_024.0;
    const GB: f64 = MB * 1_024.0;

    let rate = if bytes_per_second.is_finite() {
        bytes_per_second.max(0.0)
    } else {
        0.0
    };

    if rate >= GB {
        format!("{:.1} GB/s", rate / GB)
    } else if rate >= MB {
        format!("{:.1} MB/s", rate / MB)
    } else if rate >= KB {
        format!("{:.1} KB/s", rate / KB)
    } else {
        format!("{rate:.0} B/s")
    }
}

#[derive(Clone, Copy)]
enum MetricIcon {
    Cpu,
    Memory,
    Gpu,
}

fn metric_icon(icon: MetricIcon) -> Element<'static, Message> {
    let bytes: &'static [u8] = match icon {
        MetricIcon::Cpu => include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/cpu-symbolic.svg"
        )),
        MetricIcon::Memory => include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/memory-symbolic.svg"
        )),
        MetricIcon::Gpu => include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/gpu-symbolic.svg"
        )),
    };
    embedded_symbolic_icon(bytes, METRIC_ICON_SIZE)
}

fn temperature_item<'a>(
    label: &'a str,
    value: f32,
    style: crate::config::TemperatureGaugeStyle,
) -> Element<'a, Message> {
    widget::column::with_capacity(2)
        .align_x(Alignment::Center)
        .spacing(4)
        .push(gauge::temperature_gauge(value, style))
        .push(widget::text::heading(label))
        .into()
}

fn temperature_text_item<'a>(
    icon: MetricIcon,
    label: &'a str,
    value: f32,
    spacing: u16,
) -> Element<'a, Message> {
    widget::row::with_capacity(4)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(metric_icon(icon))
        .push(widget::text::body(label))
        .push(widget::space::horizontal())
        .push(widget::text::monotext(format!("{value:.0}°C")))
        .into()
}

pub(in crate::overlay) fn show_utilization(config: &Config) -> bool {
    config.show_cpu || config.show_memory || config.show_gpu
}

pub(in crate::overlay) fn show_temperatures(config: &Config) -> bool {
    config.show_cpu_temp || config.show_gpu_temp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_network_rates_for_compact_display() {
        assert_eq!(format_network_rate(0.0), "0 B/s");
        assert_eq!(format_network_rate(1_536.0), "1.5 KB/s");
        assert_eq!(format_network_rate(12.25 * 1_024.0 * 1_024.0), "12.2 MB/s");
        assert_eq!(format_network_rate(f64::NAN), "0 B/s");
    }
}
