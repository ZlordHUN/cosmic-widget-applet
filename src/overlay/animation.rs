// SPDX-License-Identifier: MPL-2.0

//! Shared timing for expanding, dismissing, and scrolling widget content.

use std::time::{Duration, Instant};

pub(super) const NOTIFICATION_EXPANSION_DURATION: Duration = Duration::from_millis(220);
pub(super) const NOTIFICATION_GROUP_EXPANSION_DURATION: Duration = Duration::from_millis(320);

#[derive(Debug, Clone)]
pub(super) struct ExpansionAnimation {
    pub(super) progress: f32,
    start: f32,
    pub(super) target: f32,
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
        self.start = self.progress;
        self.target = target.clamp(0.0, 1.0);
        let distance = (self.start - self.target).abs();
        self.started_at = if distance > f32::EPSILON {
            self.active_duration = self
                .duration
                .mul_f32(distance)
                .max(Duration::from_millis(48));
            Some(now)
        } else {
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
        self.progress = self.start + (self.target - self.start) * eased;

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
