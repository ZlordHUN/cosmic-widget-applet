// SPDX-License-Identifier: MPL-2.0

//! Shared timing for expanding, dismissing, and scrolling widget content.

use super::layout::{MAX_VISIBLE_NOTIFICATION_ROWS, NOTIFICATION_ITEM_HEIGHT};
use std::time::{Duration, Instant};

pub(super) const NOTIFICATION_EXPANSION_DURATION: Duration = Duration::from_millis(220);
pub(super) const NOTIFICATION_GROUP_EXPANSION_DURATION: Duration = Duration::from_millis(540);

#[derive(Debug, Clone)]
pub(super) struct ExpansionAnimation {
    pub(super) progress: f32,
    start: f32,
    pub(super) target: f32,
    animated_target: f32,
    started_at: Option<Instant>,
    duration: Duration,
    active_duration: Duration,
    linear: bool,
}

impl Default for ExpansionAnimation {
    fn default() -> Self {
        Self {
            progress: 0.0,
            start: 0.0,
            target: 0.0,
            animated_target: 0.0,
            started_at: None,
            duration: NOTIFICATION_EXPANSION_DURATION,
            active_duration: NOTIFICATION_EXPANSION_DURATION,
            linear: false,
        }
    }
}

impl ExpansionAnimation {
    pub(super) fn with_duration(duration: Duration) -> Self {
        Self {
            duration,
            active_duration: duration,
            linear: true,
            ..Self::default()
        }
    }

    pub(super) fn transition_to(&mut self, target: f32, now: Instant) {
        self.advance(now);
        self.start_transition(target, target, now);
    }

    pub(super) fn transition_group(
        &mut self,
        expanded: bool,
        children_height: f32,
        visible_fraction: f32,
        now: Instant,
    ) {
        self.advance(now);
        // The reference group reveals four ordinary children in 540 ms. Scale
        // normalized time by height so every group moves at the same px/s.
        let reference_height = (NOTIFICATION_ITEM_HEIGHT * MAX_VISIBLE_NOTIFICATION_ROWS) as f32;
        self.duration = NOTIFICATION_GROUP_EXPANSION_DURATION
            .mul_f32(children_height.max(0.0) / reference_height);
        self.linear = true;
        let visible_fraction = visible_fraction.clamp(0.0, 1.0);
        if expanded {
            // Animate only the part that can affect the viewport. Completing
            // the clipped tail in the final frame avoids needless frame ticks.
            self.start_transition(1.0, visible_fraction.max(self.progress), now);
        } else {
            // Discard only the clipped tail. The first visible frame stays in
            // place, and the remaining distance uses the same speed as opening.
            self.progress = self.progress.min(visible_fraction);
            self.start_transition(0.0, 0.0, now);
        }
    }

    fn start_transition(&mut self, target: f32, animated_target: f32, now: Instant) {
        self.start = self.progress;
        self.target = target.clamp(0.0, 1.0);
        self.animated_target = animated_target.clamp(0.0, 1.0);
        let distance = (self.start - self.animated_target).abs();
        self.started_at = if distance > f32::EPSILON {
            self.active_duration = self.duration.mul_f32(distance);
            // Linear group transitions keep their speed even for a short
            // visible remainder. Eased transitions retain their minimum time.
            if !self.linear {
                self.active_duration = self.active_duration.max(Duration::from_millis(48));
            }
            Some(now)
        } else {
            self.progress = self.target;
            None
        };
    }

    pub(super) fn advance(&mut self, now: Instant) -> bool {
        let Some(started_at) = self.started_at else {
            return false;
        };
        let linear = now.saturating_duration_since(started_at).as_secs_f32()
            / self.active_duration.as_secs_f32();
        let t = linear.clamp(0.0, 1.0);
        let eased = if self.linear {
            t
        } else {
            t * t * (3.0 - 2.0 * t)
        };
        self.progress = self.start + (self.animated_target - self.start) * eased;

        if linear >= 1.0 {
            self.progress = self.target;
            self.started_at = None;
        }

        true
    }

    pub(super) fn reset(&mut self) {
        let duration = self.duration;
        let linear = self.linear;
        *self = Self {
            duration,
            active_duration: duration,
            linear,
            ..Self::default()
        };
    }

    pub(super) fn is_animating(&self) -> bool {
        self.started_at.is_some()
    }

    pub(super) fn is_collapsed(&self) -> bool {
        !self.is_animating() && self.target == 0.0
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct ScrollAnimation {
    visual_offset: f32,
    start: f32,
    pub(super) target: f32,
    started_at: Option<Instant>,
}

impl ScrollAnimation {
    pub(super) fn transition_to(&mut self, target: f32, now: Instant) {
        self.advance(now);
        self.start = self.visual_offset;
        self.target = target.max(0.0);
        self.started_at = if (self.start - self.target).abs() > 0.5 {
            Some(now)
        } else {
            self.visual_offset = self.target;
            None
        };
    }

    pub(super) fn snap_to(&mut self, offset: f32) {
        let offset = offset.max(0.0);
        self.visual_offset = offset;
        self.start = offset;
        self.target = offset;
        self.started_at = None;
    }

    pub(super) fn advance(&mut self, now: Instant) -> bool {
        let Some(started_at) = self.started_at else {
            return false;
        };
        let linear = now.saturating_duration_since(started_at).as_secs_f32()
            / NOTIFICATION_EXPANSION_DURATION.as_secs_f32();
        let t = linear.clamp(0.0, 1.0);
        let eased = t * t * (3.0 - 2.0 * t);
        self.visual_offset = self.start + (self.target - self.start) * eased;

        if linear >= 1.0 {
            self.visual_offset = self.target;
            self.started_at = None;
        }

        true
    }

    pub(super) fn translation(&self) -> f32 {
        self.target - self.visual_offset
    }

    pub(super) fn is_animating(&self) -> bool {
        self.started_at.is_some()
    }
}
