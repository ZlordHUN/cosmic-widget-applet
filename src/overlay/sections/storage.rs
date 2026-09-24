// SPDX-License-Identifier: MPL-2.0

use crate::config::Config;
use crate::monitors::storage::DiskInfo;
use crate::overlay::Message;
use crate::overlay::components::{gauge, section::section};
use crate::overlay::stats::SystemSnapshot;
use cosmic::iced::{Alignment, Length};
use cosmic::{Element, widget};

pub(in crate::overlay) fn storage_view<'a>(
    config: &Config,
    stats: &'a SystemSnapshot,
    section_spacing: u16,
    item_spacing: u16,
) -> Element<'a, Message> {
    let mut storage = section(
        "drive-harddisk-solidstate-symbolic",
        "Storage",
        section_spacing,
    );

    if stats.disks.is_empty() {
        storage = storage.push(widget::text::caption("No mounted storage found"));
    } else {
        for disk in &stats.disks {
            storage = storage.push(storage_item(disk, config.show_percentages, item_spacing));
        }
    }

    storage.into()
}

fn storage_item<'a>(
    disk: &'a DiskInfo,
    show_percentage: bool,
    spacing: u16,
) -> Element<'a, Message> {
    let percentage = disk.used_percentage.clamp(0.0, 100.0);
    let mut title = widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(spacing)
        .push(widget::text::body(&disk.name).width(Length::Fill));

    if show_percentage {
        title = title.push(widget::text::monotext(format!("{percentage:.1}%")));
    }

    let details = if disk.is_loading || disk.total_space == 0 {
        "Loading...".to_string()
    } else {
        let used = disk.total_space.saturating_sub(disk.available_space);
        format!(
            "{} / {}",
            format_storage_bytes(used),
            format_storage_bytes(disk.total_space)
        )
    };

    widget::column::with_capacity(3)
        .spacing(spacing)
        .push(title)
        .push(gauge::indicator_bar(percentage))
        .push(
            widget::row::with_capacity(2)
                .push(widget::space::horizontal())
                .push(widget::text::caption(details)),
        )
        .into()
}

fn format_storage_bytes(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = KB * 1_000.0;
    const GB: f64 = MB * 1_000.0;
    const TB: f64 = GB * 1_000.0;

    let bytes = bytes as f64;
    if bytes >= TB {
        format!("{:.1} TB", bytes / TB)
    } else if bytes >= GB {
        format!("{:.0} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{bytes:.0} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_storage_capacities_for_compact_display() {
        assert_eq!(format_storage_bytes(1_900_000_000_000), "1.9 TB");
        assert_eq!(format_storage_bytes(608_000_000_000), "608 GB");
        assert_eq!(format_storage_bytes(950_000_000), "950 MB");
        assert_eq!(format_storage_bytes(999), "999 B");
    }
}
