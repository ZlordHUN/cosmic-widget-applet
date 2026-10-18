// SPDX-License-Identifier: MPL-2.0

//! Optimistic playback state and dismissal of outgoing media cards.

use super::animation::ExpansionAnimation;
use crate::monitors::media::{
    MultiPlayerState, PlaybackStatus, PlayerId, SubtitleState, SubtitleTarget,
};
use std::time::{Duration, Instant};

pub(super) const MEDIA_CONTROL_GRACE: Duration = Duration::from_secs(2);

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
