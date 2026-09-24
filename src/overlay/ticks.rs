// SPDX-License-Identifier: MPL-2.0

//! Align widget refreshes with wall-clock boundaries.

use cosmic::iced;
use futures_util::SinkExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) const UI_TICK_SETTLE_DELAY: Duration = Duration::from_millis(5);

pub(super) fn aligned_tick_stream(
    interval: &Duration,
) -> impl iced::futures::Stream<Item = ()> + use<> {
    let interval = *interval;

    iced::stream::channel(1, async move |mut output| {
        loop {
            let elapsed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            tokio::time::sleep(delay_until_next_tick(elapsed, interval)).await;

            if output.send(()).await.is_err() {
                break;
            }
        }
    })
}

pub(super) fn delay_until_next_tick(elapsed: Duration, interval: Duration) -> Duration {
    let interval_ns = interval.as_nanos().max(1);
    let remainder_ns = elapsed.as_nanos() % interval_ns;
    let until_boundary_ns = if remainder_ns == 0 {
        interval_ns
    } else {
        interval_ns - remainder_ns
    };
    let until_boundary = Duration::from_nanos(u64::try_from(until_boundary_ns).unwrap_or(u64::MAX));

    until_boundary.saturating_add(UI_TICK_SETTLE_DELAY)
}
