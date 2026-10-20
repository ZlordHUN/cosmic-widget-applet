// SPDX-License-Identifier: MPL-2.0

//! Optimistic playback state and dismissal of outgoing media cards.

use super::animation::ExpansionAnimation;
use crate::monitors::media::{
    MultiPlayerState, PlaybackStatus, PlayerId, SubtitleState, SubtitleTarget,
};
use std::time::{Duration, Instant};

pub(super) const MEDIA_CONTROL_GRACE: Duration = Duration::from_secs(2);
pub(super) const VOLUME_UPDATE_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub(super) struct PendingPlayback {
    pub(super) player_id: PlayerId,
    pub(super) status: PlaybackStatus,
    pub(super) expires_at: Instant,
}

#[derive(Debug, Clone)]
pub(super) struct DismissingMedia {
    pub(super) state: MultiPlayerState,
    pub(super) animation: ExpansionAnimation,
}

pub(super) fn reconcile_media_dismissal(
    dismissal: &mut Option<DismissingMedia>,
    previous: &MultiPlayerState,
    current: &MultiPlayerState,
    now: Instant,
) {
    if let Some(outgoing) = dismissal.as_ref() {
        // A source can recover while its old card is still sliding away.
        if current.current_player().is_some_and(|(id, info)| {
            info.is_active()
                && outgoing
                    .state
                    .current_player()
                    .is_some_and(|(old_id, _)| old_id == id)
        }) {
            *dismissal = None;
        }
        return;
    }

    let Some((previous_id, _)) = previous
        .current_player()
        .filter(|(_, info)| info.is_active())
    else {
        return;
    };
    if current
        .players
        .iter()
        .any(|(id, info)| id == previous_id && info.is_active())
    {
        return;
    }

    let mut animation = ExpansionAnimation::default();
    animation.transition_to(1.0, now);
    *dismissal = Some(DismissingMedia {
        state: previous.clone(),
        animation,
    });
}

pub(super) fn advance_media_dismissal(dismissal: &mut Option<DismissingMedia>, now: Instant) {
    if let Some(outgoing) = dismissal {
        outgoing.animation.advance(now);
        if !outgoing.animation.is_animating() {
            *dismissal = None;
        }
    }
}

pub(super) fn reconcile_media_state(
    state: &mut MultiPlayerState,
    pending_playback: &mut Option<PendingPlayback>,
    now: Instant,
) {
    if pending_playback
        .as_ref()
        .is_some_and(|pending| pending.expires_at <= now)
    {
        *pending_playback = None;
    }

    if let Some(pending) = pending_playback.as_ref()
        && let Some((_, info)) = state
            .players
            .iter_mut()
            .find(|(id, _)| id == &pending.player_id)
    {
        info.status = pending.status.clone();
    }
}

/// Keep drag feedback stable while sending paced updates to the owning player.
#[derive(Default)]
pub(super) struct VolumeControl {
    preview: Option<(PlayerId, f64)>,
    pending: Option<PendingVolume>,
    queued: bool,
    last_submission: Option<(PlayerId, f64, Instant)>,
}

struct PendingVolume {
    player_id: PlayerId,
    value: f64,
    expires_at: Instant,
}

impl VolumeControl {
    pub(super) fn preview(&self) -> Option<f64> {
        self.preview.as_ref().map(|(_, value)| *value)
    }

    pub(super) fn change(&mut self, state: &MultiPlayerState, target: &PlayerId, value: f64) {
        if value.is_finite() && Self::can_adjust(state, target) {
            self.preview = Some((target.clone(), value.clamp(0.0, 1.0)));
            self.queued = true;
        }
    }

    pub(super) fn has_queued_change(&self) -> bool {
        self.queued
    }

    pub(super) fn take_live_change(
        &mut self,
        state: &MultiPlayerState,
        now: Instant,
    ) -> Option<(PlayerId, f64)> {
        if !self.queued {
            return None;
        }
        let (target, value) = self.preview.as_ref()?;
        if !Self::can_adjust(state, target) {
            self.clear();
            return None;
        }
        if self.last_submission.as_ref().is_some_and(|(id, _, at)| {
            id == target && now.saturating_duration_since(*at) < VOLUME_UPDATE_INTERVAL
        }) {
            return None;
        }
        self.queued = false;
        Some((target.clone(), *value))
    }

    pub(super) fn take_change(
        &mut self,
        state: &MultiPlayerState,
        target: &PlayerId,
    ) -> Option<f64> {
        if self.preview.as_ref().is_none_or(|(id, _)| id != target) {
            return None;
        }
        let (_, value) = self.preview.take()?;
        let already_sent = !self.queued
            && self
                .last_submission
                .as_ref()
                .is_some_and(|(id, sent, _)| id == target && *sent == value);
        self.queued = false;
        (Self::can_adjust(state, target) && !already_sent).then_some(value)
    }

    pub(super) fn submitted(&mut self, target: PlayerId, value: f64, now: Instant) {
        self.last_submission = Some((target.clone(), value, now));
        self.pending = Some(PendingVolume {
            player_id: target,
            value,
            expires_at: now + MEDIA_CONTROL_GRACE,
        });
    }

    pub(super) fn clear(&mut self) {
        self.preview = None;
        self.pending = None;
        self.queued = false;
        self.last_submission = None;
    }

    pub(super) fn reconcile(&mut self, state: &mut MultiPlayerState, now: Instant) {
        if self
            .preview
            .as_ref()
            .is_some_and(|(id, _)| !Self::can_adjust(state, id))
        {
            self.clear();
            return;
        }
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        if now >= pending.expires_at || !Self::can_adjust(state, &pending.player_id) {
            self.pending = None;
            return;
        }
        let (_, info) = &mut state.players[state.current_index];
        // Emby and some MPRIS players report whole percentages.
        if info
            .volume
            .is_some_and(|value| (value - pending.value).abs() <= 0.0051)
        {
            self.pending = None;
        } else {
            // Server reports remain authoritative once the short grace expires.
            info.volume = Some(pending.value);
        }
    }

    fn can_adjust(state: &MultiPlayerState, target: &PlayerId) -> bool {
        state.current_player().is_some_and(|(id, info)| {
            id == target && info.is_active() && info.volume.is_some_and(f64::is_finite)
        })
    }
}

/// Subtitle feedback is presentation-only and expires if Emby does not acknowledge it.
#[derive(Debug, Clone)]
pub(super) struct PendingSubtitle {
    pub(super) target: SubtitleTarget,
    pub(super) index: i32,
    pub(super) expires_at: Instant,
}

pub(super) fn subtitle_toggle_index(
    subtitles: &SubtitleState,
    remembered: Option<&(SubtitleTarget, i32)>,
) -> Option<i32> {
    match subtitles.selected_index? {
        -1 => remembered
            .filter(|(target, index)| {
                target == &subtitles.target
                    && subtitles.tracks.iter().any(|track| track.index == *index)
            })
            .map(|(_, index)| *index)
            .or_else(|| subtitles.tracks.first().map(|track| track.index)),
        index if index >= 0 => Some(-1),
        _ => None,
    }
}

pub(super) fn reconcile_subtitle_state(
    state: &mut MultiPlayerState,
    pending: &mut Option<PendingSubtitle>,
    now: Instant,
) {
    let Some(selection) = pending.as_ref() else {
        return;
    };
    let subtitles = state.players.get_mut(state.current_index)
        .filter(|(id, _)| matches!(id, PlayerId::Emby(session) if session == &selection.target.session_id))
        .and_then(|(_, info)| info.subtitles.as_mut())
        .filter(|subtitles| subtitles.target == selection.target)
        .filter(|subtitles| selection.index == -1 || subtitles.tracks.iter().any(|track| track.index == selection.index));
    let Some(subtitles) = subtitles else {
        *pending = None;
        return;
    };
    if now >= selection.expires_at || subtitles.selected_index == Some(selection.index) {
        *pending = None;
    } else {
        subtitles.selected_index = Some(selection.index);
    }
}

#[cfg(test)]
mod subtitle_tests {
    use super::*;
    use crate::monitors::media::{MediaInfo, SubtitleTrack};

    fn state(selected: Option<i32>) -> MultiPlayerState {
        MultiPlayerState {
            players: vec![(
                PlayerId::Emby("session".into()),
                MediaInfo {
                    subtitles: Some(SubtitleState {
                        target: SubtitleTarget {
                            session_id: "session".into(),
                            item_id: "episode".into(),
                            media_source_id: Some("source".into()),
                        },
                        tracks: vec![
                            SubtitleTrack {
                                index: 2,
                                label: "English".into(),
                            },
                            SubtitleTrack {
                                index: 5,
                                label: "English SDH".into(),
                            },
                        ],
                        selected_index: selected,
                    }),
                    ..MediaInfo::default()
                },
            )],
            current_index: 0,
        }
    }

    #[test]
    fn cc_restores_last_enabled_track_only_for_the_same_video_source() {
        let media = state(Some(-1));
        let subtitles = media.players[0].1.subtitles.as_ref().unwrap();
        let mut remembered = (subtitles.target.clone(), 5);
        assert_eq!(subtitle_toggle_index(subtitles, Some(&remembered)), Some(5));
        remembered.0.media_source_id = Some("other".into());
        assert_eq!(subtitle_toggle_index(subtitles, Some(&remembered)), Some(2));
        remembered.0 = subtitles.target.clone();
        remembered.1 = 99;
        assert_eq!(subtitle_toggle_index(subtitles, Some(&remembered)), Some(2));
    }

    #[test]
    fn cc_does_not_guess_when_current_selection_is_unknown() {
        let mut media = state(None);
        let subtitles = media.players[0].1.subtitles.as_mut().unwrap();
        assert_eq!(subtitle_toggle_index(subtitles, None), None);
        subtitles.selected_index = Some(5);
        assert_eq!(subtitle_toggle_index(subtitles, None), Some(-1));
    }

    #[test]
    fn pending_subtitle_feedback_expires_or_accepts_the_server_acknowledgement() {
        let now = Instant::now();
        let mut media = state(Some(-1));
        let target = media.players[0]
            .1
            .subtitles
            .as_ref()
            .unwrap()
            .target
            .clone();
        let pending_selection = PendingSubtitle {
            target,
            index: 5,
            expires_at: now + MEDIA_CONTROL_GRACE,
        };
        let mut pending = Some(pending_selection.clone());
        reconcile_subtitle_state(&mut media, &mut pending, now);
        assert_eq!(
            media.players[0]
                .1
                .subtitles
                .as_ref()
                .unwrap()
                .selected_index,
            Some(5)
        );
        assert!(pending.is_some());
        media = state(Some(-1));
        reconcile_subtitle_state(&mut media, &mut pending, now + MEDIA_CONTROL_GRACE);
        assert!(pending.is_none());
        assert_eq!(
            media.players[0]
                .1
                .subtitles
                .as_ref()
                .unwrap()
                .selected_index,
            Some(-1)
        );
        media = state(Some(5));
        pending = Some(pending_selection);
        reconcile_subtitle_state(&mut media, &mut pending, now);
        assert!(pending.is_none());
    }

    #[test]
    fn pending_subtitle_selection_cannot_follow_a_new_episode_or_player() {
        let now = Instant::now();
        for change_player in [false, true] {
            let mut media = state(Some(-1));
            let target = media.players[0]
                .1
                .subtitles
                .as_ref()
                .unwrap()
                .target
                .clone();
            let mut pending = Some(PendingSubtitle {
                target,
                index: 5,
                expires_at: now + MEDIA_CONTROL_GRACE,
            });
            if change_player {
                media.players[0].0 = PlayerId::Mpris("browser".into());
            } else {
                media.players[0]
                    .1
                    .subtitles
                    .as_mut()
                    .unwrap()
                    .target
                    .item_id = "next episode".into();
            }
            reconcile_subtitle_state(&mut media, &mut pending, now);
            assert!(pending.is_none());
            assert_eq!(
                media.players[0]
                    .1
                    .subtitles
                    .as_ref()
                    .unwrap()
                    .selected_index,
                Some(-1)
            );
        }
    }
}

#[cfg(test)]
mod volume_tests {
    use super::*;
    use crate::monitors::media::MediaInfo;

    fn state() -> MultiPlayerState {
        MultiPlayerState {
            players: vec![(
                PlayerId::Cider,
                MediaInfo {
                    player_name: "Cider".into(),
                    title: "Track".into(),
                    volume: Some(0.25),
                    ..Default::default()
                },
            )],
            current_index: 0,
        }
    }

    #[test]
    fn volume_drag_keeps_preview_until_release_without_modifying_reports() {
        let mut media = state();
        let mut volume = VolumeControl::default();
        volume.change(&media, &PlayerId::Cider, 0.8);
        volume.reconcile(&mut media, Instant::now());
        assert_eq!(volume.preview(), Some(0.8));
        assert_eq!(media.players[0].1.volume, Some(0.25));
        assert_eq!(volume.take_change(&media, &PlayerId::Cider), Some(0.8));
        assert_eq!(volume.preview(), None);
        assert_eq!(volume.take_change(&media, &PlayerId::Cider), None);
    }

    #[test]
    fn volume_applies_during_drag_and_coalesces_until_the_next_interval() {
        let now = Instant::now();
        let media = state();
        let mut volume = VolumeControl::default();
        volume.change(&media, &PlayerId::Cider, 0.4);
        assert_eq!(
            volume.take_live_change(&media, now),
            Some((PlayerId::Cider, 0.4))
        );
        volume.submitted(PlayerId::Cider, 0.4, now);
        assert_eq!(volume.preview(), Some(0.4));
        assert!(!volume.has_queued_change());

        volume.change(&media, &PlayerId::Cider, 0.5);
        volume.change(&media, &PlayerId::Cider, 0.8);
        assert_eq!(
            volume.take_live_change(&media, now + Duration::from_millis(20)),
            None
        );
        assert!(volume.has_queued_change());
        // A timer flushes the last position even while the pointer is held still.
        let due = now + VOLUME_UPDATE_INTERVAL;
        assert_eq!(
            volume.take_live_change(&media, due),
            Some((PlayerId::Cider, 0.8))
        );
        volume.submitted(PlayerId::Cider, 0.8, due);
        assert_eq!(volume.preview(), Some(0.8));
        assert_eq!(volume.take_change(&media, &PlayerId::Cider), None);
        assert_eq!(volume.preview(), None);
        assert_eq!(
            volume.take_live_change(&media, due + VOLUME_UPDATE_INTERVAL),
            None
        );
    }

    #[test]
    fn volume_release_flushes_latest_level_without_waiting_for_a_timer() {
        let now = Instant::now();
        let media = state();
        let mut volume = VolumeControl::default();
        volume.change(&media, &PlayerId::Cider, 0.4);
        assert_eq!(
            volume.take_live_change(&media, now),
            Some((PlayerId::Cider, 0.4))
        );
        volume.submitted(PlayerId::Cider, 0.4, now);
        volume.change(&media, &PlayerId::Cider, 0.7);
        assert_eq!(volume.take_live_change(&media, now), None);
        // Release, keyboard, and wheel commits bypass the live-update interval.
        assert_eq!(volume.take_change(&media, &PlayerId::Cider), Some(0.7));
        volume.submitted(PlayerId::Cider, 0.7, now);
        assert!(!volume.has_queued_change());
        assert_eq!(
            volume.take_live_change(&media, now + VOLUME_UPDATE_INTERVAL),
            None
        );
    }

    #[test]
    fn queued_live_volume_is_cancelled_when_the_source_changes() {
        let now = Instant::now();
        for lost_capability in [false, true] {
            let mut media = state();
            let mut volume = VolumeControl::default();
            volume.change(&media, &PlayerId::Cider, 0.8);
            if lost_capability {
                media.players[0].1.volume = None;
            } else {
                media.players[0].0 = PlayerId::Mpris("new player".into());
            }
            assert_eq!(volume.take_live_change(&media, now), None);
            assert!(!volume.has_queued_change());
            assert_eq!(volume.preview(), None);
        }
    }

    #[test]
    fn changed_player_or_lost_capability_cancels_volume_drag() {
        for switch_player in [true, false] {
            let mut media = state();
            let mut volume = VolumeControl::default();
            volume.change(&media, &PlayerId::Cider, 0.8);
            if switch_player {
                media.players[0].0 = PlayerId::Mpris("browser".into());
            } else {
                media.players[0].1.volume = None;
            }
            assert_eq!(volume.take_change(&media, &PlayerId::Cider), None);
            volume.change(&media, &PlayerId::Cider, 0.9);
            assert_eq!(volume.preview(), None);
        }
        let mut media = state();
        let mut volume = VolumeControl::default();
        volume.change(&media, &PlayerId::Cider, 0.8);
        media.players.clear();
        volume.reconcile(&mut media, Instant::now());
        assert_eq!(volume.preview(), None);
    }

    #[test]
    fn volume_feedback_expires_or_yields_to_acknowledgement() {
        let now = Instant::now();
        let mut volume = VolumeControl::default();
        volume.submitted(PlayerId::Cider, 0.807, now);
        let mut stale = state();
        volume.reconcile(&mut stale, now);
        assert_eq!(stale.players[0].1.volume, Some(0.807));
        let mut acknowledged = state();
        acknowledged.players[0].1.volume = Some(0.81);
        volume.reconcile(&mut acknowledged, now);
        assert!(volume.pending.is_none());
        assert_eq!(acknowledged.players[0].1.volume, Some(0.81));

        volume.submitted(PlayerId::Cider, 0.8, now);
        let mut rejected = state();
        volume.reconcile(&mut rejected, now + MEDIA_CONTROL_GRACE);
        assert_eq!(rejected.players[0].1.volume, Some(0.25));
        assert!(volume.pending.is_none());
    }

    #[test]
    fn volume_preview_rejects_nonfinite_and_stale_events() {
        let media = state();
        let mut volume = VolumeControl::default();
        volume.change(&media, &PlayerId::Cider, 1.5);
        assert_eq!(volume.preview(), Some(1.0));
        volume.change(&media, &PlayerId::Cider, f64::NAN);
        volume.change(&media, &PlayerId::Mpris("old player".into()), 0.1);
        assert_eq!(volume.preview(), Some(1.0));
        assert_eq!(
            volume.take_change(&media, &PlayerId::Mpris("old player".into())),
            None
        );
        assert_eq!(volume.take_change(&media, &PlayerId::Cider), Some(1.0));
    }
}
