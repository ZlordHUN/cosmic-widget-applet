// SPDX-License-Identifier: MPL-2.0

//! Retain device rows while the card uncovers or covers their full-size contents.

use super::animation::ExpansionAnimation;
use crate::monitors::battery::BatteryDevice;
use std::time::Instant;

#[derive(Debug, Clone)]
pub(super) struct DeviceRow {
    pub(super) device: BatteryDevice,
    pub(super) animation: ExpansionAnimation,
    identity: String,
    present: bool,
}

#[derive(Debug, Clone)]
pub(super) struct DeviceList {
    rows: Vec<DeviceRow>,
    empty: ExpansionAnimation,
    initialized: bool,
    row_height: f32,
}

impl Default for DeviceList {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            empty: settled_animation(true),
            initialized: false,
            row_height: 0.0,
        }
    }
}

impl DeviceList {
    pub(super) fn reconcile(&mut self, devices: &[BatteryDevice], now: Instant, row_height: f32) {
        self.advance(now);

        if !self.initialized {
            for device in devices {
                let identity = identity(device);
                if !self.rows.iter().any(|row| row.identity == identity) {
                    self.rows.push(DeviceRow {
                        device: device.clone(),
                        animation: settled_animation(true),
                        identity,
                        present: true,
                    });
                }
            }
            self.empty = settled_animation(self.rows.is_empty());
            self.initialized = true;
            self.row_height = row_height;
            return;
        }

        // Reader ordering and battery updates do not change row positions or
        // restart transitions. A disconnected row keeps its last live contents.
        let mut topology_changed = false;
        for row in &mut self.rows {
            let incoming = devices
                .iter()
                .find(|device| identity(device) == row.identity);
            let present = incoming.is_some();
            topology_changed |= row.present != present;
            row.present = present;
            if let Some(device) = incoming {
                row.device.clone_from(device);
            }
        }
        for device in devices {
            let identity = identity(device);
            if !self.rows.iter().any(|row| row.identity == identity) {
                self.rows.push(DeviceRow {
                    device: device.clone(),
                    animation: ExpansionAnimation::default(),
                    identity,
                    present: true,
                });
                topology_changed = true;
            }
        }

        let empty = !self.rows.iter().any(|row| row.present);
        topology_changed |= (self.empty.target == 1.0) != empty;
        let geometry_changed = self.row_height != row_height;
        self.row_height = row_height;
        if topology_changed || (geometry_changed && self.is_animating()) {
            self.retime(empty, now);
        }
    }

    fn retime(&mut self, empty: bool, now: Instant) {
        // Sharing the remaining travel time prevents several simultaneous
        // additions from making the card grow faster than a notification group.
        // All rows finish together, including a row reversed partway through.
        let rows_distance: f32 = self
            .rows
            .iter()
            .map(|row| (f32::from(u8::from(row.present)) - row.animation.progress).abs())
            .sum();
        let empty_distance = (f32::from(u8::from(empty)) - self.empty.progress).abs();
        // The placeholder replaces one row, so its opposite movement should not
        // double the time when the first device arrives or the last one leaves.
        let travel = rows_distance.max(empty_distance);
        for row in &mut self.rows {
            transition(
                &mut row.animation,
                row.present,
                self.row_height,
                travel,
                now,
            );
        }
        transition(&mut self.empty, empty, self.row_height, travel, now);
    }

    pub(super) fn advance(&mut self, now: Instant) {
        for row in &mut self.rows {
            row.animation.advance(now);
        }
        self.empty.advance(now);
        self.rows
            .retain(|row| row.present || !row.animation.is_collapsed());
    }

    pub(super) fn is_animating(&self) -> bool {
        self.empty.is_animating() || self.rows.iter().any(|row| row.animation.is_animating())
    }

    pub(super) fn rows(&self) -> &[DeviceRow] {
        &self.rows
    }

    pub(super) fn empty_progress(&self) -> f32 {
        self.empty.progress
    }
}

fn identity(device: &BatteryDevice) -> String {
    device.name.trim().to_lowercase()
}

fn settled_animation(shown: bool) -> ExpansionAnimation {
    let mut animation = ExpansionAnimation::default();
    animation.progress = f32::from(u8::from(shown));
    animation.target = animation.progress;
    animation
}

fn transition(
    animation: &mut ExpansionAnimation,
    shown: bool,
    row_height: f32,
    travel: f32,
    now: Instant,
) {
    let remaining = (f32::from(u8::from(shown)) - animation.progress).abs();
    let height = if remaining > f32::EPSILON {
        row_height * travel / remaining
    } else {
        0.0
    };
    // Reuse the actual notification timing, including partial reversals, rather
    // than approximating its speed with an unrelated duration or easing curve.
    animation.transition_group(shown, height, 1.0, now);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const ROW_HEIGHT: f32 = 47.0;
    const STEP: Duration = Duration::from_millis(45);
    const ROW_DURATION: Duration = Duration::from_millis(135);

    fn device(name: &str) -> BatteryDevice {
        BatteryDevice {
            name: name.to_string(),
            level: Some(75),
            status: Some("discharging".to_string()),
            kind: Some("controller".to_string()),
            codename: None,
            is_loading: false,
            is_connected: true,
        }
    }

    fn close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
    }

    #[test]
    fn initial_rows_are_shown_without_a_startup_animation() {
        let mut list = DeviceList::default();
        assert_eq!(list.empty_progress(), 1.0);
        list.reconcile(
            &[device("Mouse"), device("Controller")],
            Instant::now(),
            ROW_HEIGHT,
        );
        assert!(!list.is_animating());
        assert_eq!(list.empty_progress(), 0.0);
        assert!(list.rows().iter().all(|row| row.animation.progress == 1.0));
    }

    #[test]
    fn disconnected_row_retains_its_contents_until_fully_covered() {
        let now = Instant::now();
        let mouse = device("Mouse");
        let controller = device("Controller");
        let mut list = DeviceList::default();
        list.reconcile(&[mouse.clone(), controller.clone()], now, ROW_HEIGHT);
        list.reconcile(&[mouse], now, ROW_HEIGHT);
        assert_eq!(list.rows().len(), 2);
        assert_eq!(list.rows()[1].device, controller);
        list.advance(now + STEP);
        close(list.rows()[1].animation.progress, 2.0 / 3.0);
        assert_eq!(list.rows()[1].device, controller);
        list.advance(now + ROW_DURATION);
        assert_eq!(list.rows().len(), 1);
        assert!(!list.is_animating());
    }

    #[test]
    fn rapid_disconnect_and_reconnect_reverse_without_duplicates_or_jumps() {
        let now = Instant::now();
        let mouse = device("Mouse");
        let controller = device("Controller");
        let mut list = DeviceList::default();
        list.reconcile(&[mouse.clone()], now, ROW_HEIGHT);
        list.reconcile(&[mouse.clone(), controller.clone()], now, ROW_HEIGHT);
        list.advance(now + STEP);
        let before_disconnect = list.rows()[1].animation.progress;
        list.reconcile(&[mouse.clone()], now + STEP, ROW_HEIGHT);
        close(list.rows()[1].animation.progress, before_disconnect);
        list.advance(now + STEP + Duration::from_millis(15));
        let before_reconnect = list.rows()[1].animation.progress;
        list.reconcile(
            &[controller, mouse],
            now + STEP + Duration::from_millis(15),
            ROW_HEIGHT,
        );
        assert_eq!(list.rows().len(), 2);
        assert_eq!(list.rows()[1].device.name, "Controller");
        close(list.rows()[1].animation.progress, before_reconnect);
        list.advance(now + Duration::from_secs(1));
        assert_eq!(list.rows()[1].animation.progress, 1.0);
        assert!(!list.is_animating());
    }

    #[test]
    fn repeated_readings_update_live_data_without_restarting_or_reordering() {
        let now = Instant::now();
        let mouse = device("Mouse");
        let mut controller = device("Controller");
        let mut list = DeviceList::default();
        list.reconcile(&[mouse.clone()], now, ROW_HEIGHT);
        list.reconcile(&[mouse.clone(), controller.clone()], now, ROW_HEIGHT);
        controller.name = "CONTROLLER".to_string();
        controller.level = Some(76);
        controller.status = Some("charging".to_string());
        list.reconcile(&[controller.clone(), mouse], now + STEP, ROW_HEIGHT);
        assert_eq!(list.rows()[0].device.name, "Mouse");
        assert_eq!(list.rows()[1].device, controller);
        close(list.rows()[1].animation.progress, 1.0 / 3.0);
        list.advance(now + STEP * 2);
        close(list.rows()[1].animation.progress, 2.0 / 3.0);
        list.advance(now + ROW_DURATION);
        assert!(!list.is_animating());
    }

    #[test]
    fn placeholder_exchanges_height_with_first_and_last_device() {
        let now = Instant::now();
        let controller = device("Controller");
        let mut list = DeviceList::default();
        list.reconcile(&[], now, ROW_HEIGHT);
        list.reconcile(&[controller], now, ROW_HEIGHT);
        list.advance(now + STEP);
        close(
            list.empty_progress() + list.rows()[0].animation.progress,
            1.0,
        );
        list.advance(now + ROW_DURATION);
        assert_eq!(list.empty_progress(), 0.0);
        let disconnected = now + Duration::from_secs(1);
        list.reconcile(&[], disconnected, ROW_HEIGHT);
        list.advance(disconnected + STEP);
        close(
            list.empty_progress() + list.rows()[0].animation.progress,
            1.0,
        );
        list.advance(disconnected + ROW_DURATION);
        assert!(list.rows().is_empty());
        assert_eq!(list.empty_progress(), 1.0);
        assert!(!list.is_animating());
    }

    #[test]
    fn adding_and_removing_several_devices_matches_notification_group_speed() {
        let now = Instant::now();
        let mouse = device("Mouse");
        let devices = [
            mouse.clone(),
            device("Controller"),
            device("Headset"),
            device("Keyboard"),
        ];
        let mut list = DeviceList::default();
        list.reconcile(&[mouse.clone()], now, ROW_HEIGHT);
        list.reconcile(&devices, now, ROW_HEIGHT);
        let mut notification = ExpansionAnimation::default();
        notification.transition_group(true, ROW_HEIGHT * 3.0, 1.0, now);
        list.advance(now + STEP);
        notification.advance(now + STEP);
        for row in &list.rows()[1..] {
            close(row.animation.progress, notification.progress);
        }
        let expanded = now + Duration::from_secs(1);
        list.advance(expanded);
        notification.advance(expanded);
        list.reconcile(&[mouse], expanded, ROW_HEIGHT);
        notification.transition_group(false, ROW_HEIGHT * 3.0, 1.0, expanded);
        list.advance(expanded + STEP);
        notification.advance(expanded + STEP);
        for row in &list.rows()[1..] {
            close(row.animation.progress, notification.progress);
        }
        list.advance(expanded + Duration::from_secs(1));
        assert_eq!(list.rows().len(), 1);
        assert!(!list.is_animating());
    }

    #[test]
    fn adding_a_row_mid_transition_preserves_progress_and_shared_speed() {
        let now = Instant::now();
        let mouse = device("Mouse");
        let controller = device("Controller");
        let mut list = DeviceList::default();
        list.reconcile(&[mouse.clone()], now, ROW_HEIGHT);
        list.reconcile(&[mouse.clone(), controller.clone()], now, ROW_HEIGHT);
        list.advance(now + STEP);
        let first_progress = list.rows()[1].animation.progress;
        list.reconcile(
            &[mouse, controller, device("Headset")],
            now + STEP,
            ROW_HEIGHT,
        );
        close(list.rows()[1].animation.progress, first_progress);
        assert_eq!(list.rows()[2].animation.progress, 0.0);
        list.advance(now + STEP * 2);
        let revealed = list.rows()[1].animation.progress + list.rows()[2].animation.progress;
        close(revealed - first_progress, 1.0 / 3.0);
        list.advance(now + Duration::from_secs(1));
        assert!(list.rows().iter().all(|row| row.animation.progress == 1.0));
        assert!(!list.is_animating());
    }
}
