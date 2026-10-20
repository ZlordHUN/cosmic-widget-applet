// SPDX-License-Identifier: MPL-2.0

//! Player-scoped volume readbacks and queued controls.

use super::{EmbyControl, EmbySession, MediaMonitor, MultiPlayerState, PlayerId, cider, mpris};
use serde::{Deserialize, Deserializer};
use std::sync::{Arc, Mutex};

pub(super) fn valid_level(level: f64) -> bool {
    level.is_finite() && (0.0..=1.0).contains(&level)
}

// A broken optional volume field must not discard otherwise usable playback metadata.
pub(super) fn deserialize_number<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<f64>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_f64().filter(|level| level.is_finite()))
}

pub(super) fn from_emby_session(session: &EmbySession) -> Option<f64> {
    if !session.supports_remote_control
        || !session
            .supported_commands
            .iter()
            .any(|command| command.eq_ignore_ascii_case("SetVolume"))
    {
        return None;
    }
    session
        .play_state
        .as_ref()?
        .volume_level
        .filter(|level| (0.0..=100.0).contains(level))
        .map(|level| level / 100.0)
}

fn valid_target(state: &MultiPlayerState, target: &PlayerId) -> bool {
    state.current_player().is_some_and(|(id, info)| {
        id == target && info.is_active() && info.volume.is_some_and(valid_level)
    })
}

enum Destination {
    Cider { token: Option<String> },
    Emby(EmbyControl),
    Mpris,
}

pub(super) struct Command {
    target: PlayerId,
    level: f64,
    destination: Destination,
}

impl Command {
    pub(super) fn prepare(monitor: &MediaMonitor, target: &PlayerId, level: f64) -> Option<Self> {
        if !valid_level(level) || !valid_target(&monitor.player_state.lock().unwrap(), target) {
            return None;
        }
        let destination = match target {
            PlayerId::Cider => Destination::Cider {
                token: monitor.cider_token.lock().unwrap().clone(),
            },
            PlayerId::Emby(session_id) => {
                let control = monitor.emby_control.lock().unwrap().as_ref()?.clone();
                if &control.session_id != session_id {
                    return None;
                }
                Destination::Emby(control)
            }
            PlayerId::Mpris(_) => Destination::Mpris,
        };
        Some(Self {
            target: target.clone(),
            level,
            destination,
        })
    }

    fn valid(&self, state: &MultiPlayerState, emby_control: Option<&EmbyControl>) -> bool {
        if !valid_level(self.level) || !valid_target(state, &self.target) {
            return false;
        }
        match &self.destination {
            Destination::Emby(control) => emby_control.is_some_and(|current| {
                current.session_id == control.session_id
                    && current.server_url == control.server_url
                    && current.access_token == control.access_token
                    && self.target == PlayerId::Emby(control.session_id.clone())
            }),
            Destination::Cider { .. } => self.target == PlayerId::Cider,
            Destination::Mpris => matches!(self.target, PlayerId::Mpris(_)),
        }
    }

    pub(super) fn dispatch(
        self,
        cider: &cider::Client,
        mpris: &mpris::Monitor,
        emby_client: Option<&reqwest::blocking::Client>,
        player_state: &Arc<Mutex<MultiPlayerState>>,
        emby_control: &Arc<Mutex<Option<EmbyControl>>>,
    ) {
        // A drag may finish after the user changes players or one disappears.
        // Never redirect that action to whichever source happens to be selected.
        let valid = {
            let state = player_state.lock().unwrap();
            let control = emby_control.lock().unwrap();
            self.valid(&state, control.as_ref())
        };
        if !valid {
            return;
        }
        let succeeded = match &self.destination {
            Destination::Cider { token } => cider.set_volume(self.level, token.as_deref()),
            Destination::Emby(control) => {
                emby_client.is_some_and(|client| send_emby(client, control, self.level))
            }
            Destination::Mpris => match &self.target {
                PlayerId::Mpris(bus_name) => mpris.set_volume(bus_name, self.level),
                _ => false,
            },
        };
        if !succeeded {
            log::debug!(
                "Media volume command failed for {}",
                self.target.display_name()
            );
        }
        // Leave the monitor authoritative: subsequent normal source updates
        // report the player's actual volume, even when a write was ignored.
    }
}

fn send_emby(client: &reqwest::blocking::Client, control: &EmbyControl, level: f64) -> bool {
    if !valid_level(level)
        || control.session_id.is_empty()
        || !control
            .session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return false;
    }
    let percentage = (level * 100.0).round() as u8;
    client
        .post(MediaMonitor::emby_api_url(
            &control.server_url,
            &format!("Sessions/{}/Command/SetVolume", control.session_id),
        ))
        .header("X-Emby-Token", &control.access_token)
        .json(&serde_json::json!({"Arguments": {"Volume": percentage.to_string()}}))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .is_ok()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn emby_volume_requires_reported_level_and_supported_control() {
        let mut fixture = json!({
            "Id":"session-1", "Client":"Emby Theater", "DeviceName":"desktop",
            "SupportsRemoteControl":true, "SupportedCommands":["SetVolume"],
            "NowPlayingItem":{"Id":"movie-1", "Name":"Movie"},
            "PlayState":{"VolumeLevel":37}
        });
        let parse = |value| serde_json::from_value::<EmbySession>(value).unwrap();
        assert_eq!(from_emby_session(&parse(fixture.clone())), Some(0.37));
        fixture["SupportsRemoteControl"] = json!(false);
        assert_eq!(from_emby_session(&parse(fixture.clone())), None);
        fixture["SupportsRemoteControl"] = json!(true);
        fixture["SupportedCommands"] = json!(["VolumeUp", "VolumeDown"]);
        assert_eq!(from_emby_session(&parse(fixture.clone())), None);
        fixture["SupportedCommands"] = json!(["SetVolume"]);
        for invalid in [json!(null), json!(-1), json!(101), json!("50"), json!({})] {
            fixture["PlayState"]["VolumeLevel"] = invalid;
            assert_eq!(from_emby_session(&parse(fixture.clone())), None);
        }
    }

    #[test]
    fn queued_volume_is_bound_to_selected_player_and_keeps_reported_state() {
        let (monitor, receiver) = super::super::tests::seek_monitor(20_000, 90_000);
        let target = {
            let mut state = monitor.player_state.lock().unwrap();
            state.players[1].1.volume = Some(0.3);
            state.players[1].0.clone()
        };
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 1.1] {
            assert!(!monitor.set_volume(&target, invalid));
        }
        assert!(!monitor.set_volume(&PlayerId::Cider, 0.5));
        assert!(monitor.set_volume(&target, 0.75));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            super::super::MediaControlCommand::Volume
        ));
        let command = monitor
            .controls
            .pending_volume
            .lock()
            .unwrap()
            .take()
            .unwrap();
        assert_eq!(command.target, target);
        assert_eq!(command.level, 0.75);
        let mut state = monitor.player_state.lock().unwrap();
        assert_eq!(state.players[1].1.volume, Some(0.3));
        assert!(command.valid(&state, None));
        state.current_index = 0;
        assert!(!command.valid(&state, None));
        state.current_index = 1;
        state.players[1].1.volume = None;
        assert!(!command.valid(&state, None));
        state.players[1].1.volume = Some(0.3);
        state.players[1].1.status = super::super::PlaybackStatus::Stopped;
        assert!(command.valid(&state, None));
        state.players[1].1.title.clear();
        assert!(!command.valid(&state, None));
        state.players.clear();
        assert!(!command.valid(&state, None));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn volume_bursts_keep_only_the_latest_level_and_one_wake() {
        let (monitor, receiver) = super::super::tests::seek_monitor(20_000, 90_000);
        let target = {
            let mut state = monitor.player_state.lock().unwrap();
            state.players[1].1.volume = Some(0.3);
            state.players[1].0.clone()
        };
        for level in 0..=100 {
            assert!(monitor.set_volume(&target, f64::from(level) / 100.0));
        }
        assert!(matches!(
            receiver.try_recv().unwrap(),
            super::super::MediaControlCommand::Volume
        ));
        assert!(receiver.try_recv().is_err());
        let in_flight = monitor
            .controls
            .pending_volume
            .lock()
            .unwrap()
            .take()
            .unwrap();
        assert_eq!(in_flight.level, 1.0);

        // While that request is in flight, another burst gets one new wake
        // and leaves the released value ready for the worker's next turn.
        for level in (25..=75).rev() {
            assert!(monitor.set_volume(&target, f64::from(level) / 100.0));
        }
        assert!(matches!(
            receiver.try_recv().unwrap(),
            super::super::MediaControlCommand::Volume
        ));
        assert!(receiver.try_recv().is_err());
        let final_change = monitor
            .controls
            .pending_volume
            .lock()
            .unwrap()
            .take()
            .unwrap();
        assert_eq!(final_change.target, target);
        assert_eq!(final_change.level, 0.25);
    }

    #[test]
    fn full_control_queue_still_accepts_the_final_volume() {
        let (monitor, receiver) = super::super::tests::seek_monitor(20_000, 90_000);
        let target = {
            let mut state = monitor.player_state.lock().unwrap();
            state.players[1].1.volume = Some(0.3);
            state.players[1].0.clone()
        };
        for _ in 0..4 {
            assert!(monitor.seek_relative(1_000));
        }
        assert!(monitor.set_volume(&target, 0.5));
        assert!(monitor.set_volume(&target, 0.8));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            super::super::MediaControlCommand::Mpris { .. }
        ));
        // The worker drains this slot on any received command; a dedicated
        // volume wake is unnecessary when playback commands filled the queue.
        let command = monitor
            .controls
            .pending_volume
            .lock()
            .unwrap()
            .take()
            .unwrap();
        assert_eq!(command.target, target);
        assert_eq!(command.level, 0.8);
        assert_eq!(receiver.try_iter().count(), 3);
    }

    #[test]
    fn unavailable_worker_rejects_volume_and_clears_pending_change() {
        let (monitor, receiver) = super::super::tests::seek_monitor(20_000, 90_000);
        let target = {
            let mut state = monitor.player_state.lock().unwrap();
            state.players[1].1.volume = Some(0.3);
            state.players[1].0.clone()
        };
        drop(receiver);
        assert!(!monitor.set_volume(&target, 0.8));
        assert!(monitor.controls.pending_volume.lock().unwrap().is_none());
    }

    #[test]
    fn emby_volume_rechecks_session_and_server_credentials_before_dispatch() {
        let (monitor, receiver, target) = super::super::tests::subtitle_monitor();
        monitor.player_state.lock().unwrap().players[1].1.volume = Some(0.5);
        assert!(monitor.set_volume(&PlayerId::Emby(target.session_id), 0.25));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            super::super::MediaControlCommand::Volume
        ));
        let command = monitor
            .controls
            .pending_volume
            .lock()
            .unwrap()
            .take()
            .unwrap();
        let state = monitor.player_state.lock().unwrap();
        let mut control = monitor.emby_control.lock().unwrap().clone().unwrap();
        assert!(command.valid(&state, Some(&control)));
        assert!(!command.valid(&state, None));
        control.access_token = "changed-token".into();
        assert!(!command.valid(&state, Some(&control)));
        control.access_token = "fixture-token".into();
        control.server_url = "http://different-server.invalid".into();
        assert!(!command.valid(&state, Some(&control)));
        control.server_url = "http://test.invalid".into();
        control.session_id = "different-session".into();
        assert!(!command.valid(&state, Some(&control)));
    }

    #[test]
    fn emby_volume_posts_percentage_as_a_general_command() {
        let (url, request) = http_fixture(204, "");
        let control = EmbyControl {
            server_url: url,
            access_token: "fixture-token".into(),
            session_id: "session-1".into(),
            subtitle_target: None,
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();
        assert!(send_emby(&client, &control, 0.376));
        let (headers, body) = request.join().unwrap();
        assert!(headers.starts_with("POST /emby/Sessions/session-1/Command/SetVolume HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("x-emby-token: fixture-token")
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            json!({"Arguments":{"Volume":"38"}})
        );
    }

    pub(in crate::monitors::media) fn http_fixture(
        status: u16,
        body: &str,
    ) -> (String, std::thread::JoinHandle<(String, Vec<u8>)>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let body = body.to_string();
        let request = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            let end = loop {
                let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0, "request ended before headers");
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while bytes.len() < end + 4 + length {
                let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0, "request ended before body");
                bytes.extend_from_slice(&buffer[..count]);
            }
            write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            (headers, bytes[end + 4..end + 4 + length].to_vec())
        });
        (url, request)
    }
}
