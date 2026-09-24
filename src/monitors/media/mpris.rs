// SPDX-License-Identifier: MPL-2.0

//! Native MPRIS discovery, state monitoring, and playback control.

use super::{MediaInfo, PlaybackStatus, PlayerId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const RECONCILE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(super) struct Monitor {
    players: Arc<Mutex<HashMap<String, MediaInfo>>>,
    connection: Arc<Mutex<Option<Connection>>>,
    refresh_lock: Arc<Mutex<()>>,
}

impl Monitor {
    pub(super) fn new() -> Self {
        let monitor = Self {
            players: Arc::new(Mutex::new(HashMap::new())),
            connection: Arc::new(Mutex::new(None)),
            refresh_lock: Arc::new(Mutex::new(())),
        };

        let listener = monitor.clone();
        std::thread::spawn(move || listener.supervise_connection());

        // Signals are the primary update path. This slow reconciliation handles
        // players that omit a signal or appear during a session-bus race.
        let reconciler = monitor.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(RECONCILE_INTERVAL);
                reconciler.refresh();
            }
        });

        monitor
    }

    pub(super) fn players(&self) -> Vec<(String, MediaInfo)> {
        let mut players = self
            .players
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(name, info)| (name.clone(), info.clone()))
            .collect::<Vec<_>>();
        players.sort_by(|left, right| left.0.cmp(&right.0));
        players
    }

    pub(super) fn refresh_timeline(&self, bus_name: &str) -> Option<MediaInfo> {
        log::info!("Refreshing incomplete Firefox MPRIS timeline");
        if !self.pause(bus_name) {
            return None;
        }

        let connection = self.connection()?;
        let mut refreshed = None;
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(150));
            refreshed = query_player(&connection, bus_name).ok().flatten();
            if refreshed.as_ref().is_some_and(|info| info.duration > 0) {
                break;
            }
        }

        let resumed = self.play(bus_name);
        if !resumed {
            log::warn!("Failed to resume Firefox after refreshing its MPRIS timeline");
        }
        if resumed && let Some(info) = refreshed.as_mut() {
            info.status = PlaybackStatus::Playing;
            self.upsert(bus_name, info.clone());
        }
        refreshed
    }

    pub(super) fn play_pause(&self, bus_name: &str) -> bool {
        let desired = self
            .players
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(bus_name)
            .map_or(PlaybackStatus::Playing, |info| match info.status {
                PlaybackStatus::Playing => PlaybackStatus::Paused,
                _ => PlaybackStatus::Playing,
            });

        if !self.call_no_args(bus_name, "PlayPause") {
            return false;
        }
        self.set_status(bus_name, desired);
        true
    }

    pub(super) fn next(&self, bus_name: &str) -> bool {
        self.call_no_args(bus_name, "Next")
    }

    pub(super) fn previous(&self, bus_name: &str) -> bool {
        self.call_no_args(bus_name, "Previous")
    }

    pub(super) fn seek(&self, bus_name: &str, position_us: u64) -> bool {
        let Some(connection) = self.connection() else {
            return false;
        };
        let Ok(proxy) = Proxy::new(&connection, bus_name, MPRIS_PATH, PLAYER_INTERFACE) else {
            return false;
        };
        let target = i64::try_from(position_us).unwrap_or(i64::MAX);
        let properties = query_properties(&connection, bus_name).ok();

        let absolute_seek = properties
            .as_ref()
            .and_then(metadata)
            .and_then(|metadata| metadata_object_path(&metadata, "mpris:trackid"))
            .is_some_and(|track_id| {
                proxy
                    .call::<_, _, ()>("SetPosition", &(track_id, target))
                    .is_ok()
            });

        let succeeded = absolute_seek
            || properties
                .as_ref()
                .and_then(|properties| property_i64(properties, "Position"))
                .is_some_and(|current| {
                    proxy
                        .call::<_, _, ()>("Seek", &(target.saturating_sub(current),))
                        .is_ok()
                });

        if succeeded {
            self.set_position(bus_name, position_us / 1000);
        }
        succeeded
    }

    fn supervise_connection(&self) {
        loop {
            match Connection::session() {
                Ok(connection) => {
                    if let Err(error) = self.monitor_connection(connection) {
                        log::warn!("Native MPRIS connection ended: {error}");
                    }
                }
                Err(error) => log::warn!("Native MPRIS connection unavailable: {error}"),
            }

            *self
                .connection
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            std::thread::sleep(RECONNECT_DELAY);
        }
    }

    fn monitor_connection(
        &self,
        signal_connection: Connection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Keep the blocking signal iterator isolated from synchronous method
        // replies. Sharing one connection lets the iterator consume a reply
        // before the corresponding control/property call sees it.
        let command_connection = Connection::session()?;
        let bus = zbus::blocking::fdo::DBusProxy::new(&signal_connection)?;
        let player_signals = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .path(MPRIS_PATH)?
            .build();
        let owner_changes = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg0ns("org.mpris.MediaPlayer2")?
            .build();
        bus.add_match_rule(player_signals)?;
        bus.add_match_rule(owner_changes)?;

        let mut messages = MessageIterator::from(&signal_connection);
        *self
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(command_connection.clone());
        self.refresh_with(&command_connection)?;
        log::info!("Using native zbus MPRIS monitoring");

        for message in &mut messages {
            message?;
            if let Err(error) = self.refresh_with(&command_connection) {
                log::debug!("Native MPRIS signal refresh failed: {error}");
            }
        }
        Err("MPRIS signal stream closed".into())
    }

    fn refresh(&self) {
        let Some(connection) = self.connection() else {
            return;
        };
        if let Err(error) = self.refresh_with(&connection) {
            log::debug!("Native MPRIS reconciliation failed: {error}");
        }
    }

    fn refresh_with(&self, connection: &Connection) -> zbus::Result<()> {
        let _refresh = self
            .refresh_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let bus = zbus::blocking::fdo::DBusProxy::new(connection)?;
        let mut names = bus
            .list_names()?
            .into_iter()
            .map(|name| name.to_string())
            .filter(|name| name.starts_with(MPRIS_PREFIX))
            .collect::<Vec<_>>();
        names.sort();

        let previous = self
            .players
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let mut refreshed = HashMap::new();
        for name in names {
            match query_player(connection, &name) {
                Ok(Some(info)) => {
                    refreshed.insert(name, info);
                }
                Ok(None) => {}
                Err(error) => {
                    log::debug!("Failed to query native MPRIS player {name}: {error}");
                    if let Some(info) = previous.get(&name) {
                        refreshed.insert(name, info.clone());
                    }
                }
            }
        }

        *self
            .players
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = refreshed;
        Ok(())
    }

    fn connection(&self) -> Option<Connection> {
        self.connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn call_no_args(&self, bus_name: &str, method: &str) -> bool {
        let Some(connection) = self.connection() else {
            return false;
        };
        let Ok(proxy) = Proxy::new(&connection, bus_name, MPRIS_PATH, PLAYER_INTERFACE) else {
            return false;
        };
        match proxy.call::<_, _, ()>(method, &()) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("MPRIS {method} failed for {bus_name}: {error}");
                false
            }
        }
    }

    fn pause(&self, bus_name: &str) -> bool {
        if self.call_no_args(bus_name, "Pause") {
            self.set_status(bus_name, PlaybackStatus::Paused);
            true
        } else {
            false
        }
    }

    fn play(&self, bus_name: &str) -> bool {
        if self.call_no_args(bus_name, "Play") {
            self.set_status(bus_name, PlaybackStatus::Playing);
            true
        } else {
            false
        }
    }

    fn set_status(&self, bus_name: &str, status: PlaybackStatus) {
        if let Some(info) = self
            .players
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(bus_name)
        {
            info.status = status;
        }
    }

    fn set_position(&self, bus_name: &str, position: u64) {
        if let Some(info) = self
            .players
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(bus_name)
        {
            info.position = position;
        }
    }

    fn upsert(&self, bus_name: &str, info: MediaInfo) {
        self.players
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(bus_name.to_string(), info);
    }
}

fn query_player(connection: &Connection, bus_name: &str) -> zbus::Result<Option<MediaInfo>> {
    let properties = query_properties(connection, bus_name)?;
    Ok(media_info_from_properties(bus_name, &properties))
}

fn query_properties(
    connection: &Connection,
    bus_name: &str,
) -> zbus::Result<HashMap<String, OwnedValue>> {
    let properties = Proxy::new(connection, bus_name, MPRIS_PATH, PROPERTIES_INTERFACE)?;
    properties.call("GetAll", &(PLAYER_INTERFACE,))
}

fn media_info_from_properties(
    bus_name: &str,
    properties: &HashMap<String, OwnedValue>,
) -> Option<MediaInfo> {
    let metadata = metadata(properties)?;
    let title = metadata_string(&metadata, "xesam:title").unwrap_or_default();
    if title.is_empty() {
        return None;
    }

    let status = match property_string(properties, "PlaybackStatus").as_deref() {
        Some("Playing") => PlaybackStatus::Playing,
        Some("Paused") => PlaybackStatus::Paused,
        _ => PlaybackStatus::Stopped,
    };
    let can_control = property_bool(properties, "CanControl").unwrap_or(true);

    Some(MediaInfo {
        player_name: PlayerId::Mpris(bus_name.to_string()).display_name(),
        title,
        artist: metadata_string_array(&metadata, "xesam:artist").unwrap_or_default(),
        album: metadata_string(&metadata, "xesam:album").unwrap_or_default(),
        art_url: metadata_string(&metadata, "mpris:artUrl"),
        media_url: metadata_string(&metadata, "xesam:url"),
        album_art: None,
        status,
        position: property_i64(properties, "Position").unwrap_or(0).max(0) as u64 / 1000,
        duration: metadata_i64(&metadata, "mpris:length").unwrap_or(0).max(0) as u64 / 1000,
        can_play: can_control && property_bool(properties, "CanPlay").unwrap_or(true),
        can_pause: can_control && property_bool(properties, "CanPause").unwrap_or(true),
        can_go_next: can_control && property_bool(properties, "CanGoNext").unwrap_or(true),
        can_go_previous: can_control && property_bool(properties, "CanGoPrevious").unwrap_or(true),
        can_seek: can_control && property_bool(properties, "CanSeek").unwrap_or(false),
    })
}

fn metadata(properties: &HashMap<String, OwnedValue>) -> Option<HashMap<String, OwnedValue>> {
    HashMap::try_from(properties.get("Metadata")?.try_clone().ok()?).ok()
}

fn property_string(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    value_string(properties.get(key)?)
}

fn property_bool(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    bool::try_from(properties.get(key)?).ok()
}

fn property_i64(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<i64> {
    i64::try_from(properties.get(key)?).ok()
}

fn metadata_string(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    value_string(metadata.get(key)?)
}

fn metadata_string_array(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    Vec::<String>::try_from(metadata.get(key)?.try_clone().ok()?)
        .ok()?
        .into_iter()
        .next()
}

fn metadata_i64(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<i64> {
    i64::try_from(metadata.get(key)?).ok()
}

fn metadata_object_path(
    metadata: &HashMap<String, OwnedValue>,
    key: &str,
) -> Option<OwnedObjectPath> {
    OwnedObjectPath::try_from(metadata.get(key)?.try_clone().ok()?).ok()
}

fn value_string(value: &OwnedValue) -> Option<String> {
    <&str>::try_from(value).ok().map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    fn owned(value: Value<'_>) -> OwnedValue {
        OwnedValue::try_from(value).unwrap()
    }

    #[test]
    fn parses_structured_player_properties_and_capabilities() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "xesam:title".to_string(),
            owned(Value::from("Intel: \"Ya, we're cooked\"")),
        );
        metadata.insert(
            "xesam:artist".to_string(),
            owned(Value::from(vec!["TechLinked".to_string()])),
        );
        metadata.insert(
            "mpris:length".to_string(),
            OwnedValue::from(559_000_000_i64),
        );

        let mut properties = HashMap::new();
        properties.insert("Metadata".to_string(), OwnedValue::from(metadata));
        properties.insert("PlaybackStatus".to_string(), owned(Value::from("Playing")));
        properties.insert("Position".to_string(), OwnedValue::from(125_000_000_i64));
        properties.insert("CanControl".to_string(), OwnedValue::from(true));
        properties.insert("CanSeek".to_string(), OwnedValue::from(true));

        let info =
            media_info_from_properties("org.mpris.MediaPlayer2.firefox.instance_1", &properties)
                .unwrap();
        assert_eq!(info.player_name, "Firefox");
        assert_eq!(info.title, "Intel: \"Ya, we're cooked\"");
        assert_eq!(info.artist, "TechLinked");
        assert_eq!(info.position, 125_000);
        assert_eq!(info.duration, 559_000);
        assert_eq!(info.status, PlaybackStatus::Playing);
        assert!(info.can_seek);
    }

    #[test]
    fn rejects_players_without_track_metadata() {
        let properties = HashMap::new();
        assert!(media_info_from_properties("org.mpris.MediaPlayer2.empty", &properties).is_none());
    }

    #[test]
    #[ignore = "requires an active desktop session and MPRIS player"]
    fn discovers_live_session_players() {
        let monitor = Monitor::new();
        let players = wait_for_live_players(&monitor);

        for (name, info) in &players {
            eprintln!(
                "{name}: {:?}, {} / {} ms, can_seek={}",
                info.status, info.position, info.duration, info.can_seek
            );
        }
    }

    #[test]
    #[ignore = "requires an active desktop session and seekable MPRIS player"]
    fn seeks_live_player_to_its_current_position() {
        let monitor = Monitor::new();
        let players = wait_for_live_players(&monitor);
        let (name, info) = players
            .iter()
            .find(|(name, info)| name.contains("firefox") && info.can_seek && info.duration > 0)
            .or_else(|| {
                players
                    .iter()
                    .find(|(_, info)| info.can_seek && info.duration > 0)
            })
            .expect("no seekable live MPRIS player was discovered");

        assert!(monitor.seek(name, info.position.saturating_mul(1000)));
    }

    fn wait_for_live_players(monitor: &Monitor) -> Vec<(String, MediaInfo)> {
        (0..30)
            .find_map(|_| {
                let players = monitor.players();
                if players.is_empty() {
                    std::thread::sleep(Duration::from_millis(100));
                    None
                } else {
                    Some(players)
                }
            })
            .expect("no live MPRIS players were discovered")
    }
}
