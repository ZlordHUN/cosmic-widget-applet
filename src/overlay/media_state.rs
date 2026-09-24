// SPDX-License-Identifier: MPL-2.0

//! Optimistic playback state and dismissal of outgoing media cards.

use super::animation::ExpansionAnimation;
use crate::monitors::media::{MultiPlayerState, PlaybackStatus, PlayerId};
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
