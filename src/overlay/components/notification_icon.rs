// SPDX-License-Identifier: MPL-2.0

//! Cached application icons for notification groups and individual rows.

use crate::monitors::notifications::Notification;
use crate::overlay::{Message, notification_state::notification_source};
use cosmic::iced::Length;
use cosmic::{Element, widget};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub(in crate::overlay) const SIZE: u16 = 20;
const CACHE_LIMIT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum App {
    Instagram,
    Discord,
    Files,
    Firmware,
    Store,
    QBittorrent,
}

impl App {
    fn from_identity(identity: &str) -> Option<Self> {
        match identity
            .trim()
            .trim_end_matches(".desktop")
            .to_ascii_lowercase()
            .as_str()
        {
            "instagram" => Some(Self::Instagram),
            "discord"
            | "discordcanary"
            | "discordptb"
            | "discord-canary"
            | "discord-ptb"
            | "com.discordapp.discord"
            | "vesktop"
            | "dev.vencord.vesktop" => Some(Self::Discord),
            "cosmic files" | "cosmic-files" | "com.system76.cosmicfiles" => Some(Self::Files),
            "firmware manager"
            | "firmware-manager"
            | "com.system76.firmwaremanager"
            | "firmware updates are available" => Some(Self::Firmware),
            "cosmic store" | "cosmic-store" | "com.system76.cosmicstore" => Some(Self::Store),
            "qbittorrent" | "org.qbittorrent.qbittorrent" => Some(Self::QBittorrent),
            _ => None,
        }
    }

    fn icon_names(self) -> &'static [&'static str] {
        match self {
            Self::Instagram => &[], // Embedded: browser notifications need no installed app.
            Self::Discord => &[
                "com.discordapp.Discord",
                "discord",
                "discord-canary",
                "discord-ptb",
                "dev.vencord.Vesktop",
                "vesktop",
            ],
            Self::Files => &["com.system76.CosmicFiles", "cosmic-files", "folder"],
            Self::Firmware => &["firmware-manager", "application-x-firmware"],
            Self::Store => &["com.system76.CosmicStore", "cosmic-store"],
            Self::QBittorrent => &["org.qbittorrent.qBittorrent", "qbittorrent"],
        }
    }
}

#[derive(PartialEq, Eq, Hash)]
struct IconKey {
    app: Option<App>,
    app_icon: Option<String>,
    desktop_entry: Option<String>,
    app_name: String,
}

fn known_app(notification: &Notification) -> Option<App> {
    // Discord's visible app name may be a conversation name. Prefer the real
    // desktop identity, then use source labels for web apps such as Instagram.
    notification
        .desktop_entry
        .as_deref()
        .and_then(App::from_identity)
        .or_else(|| App::from_identity(notification_source(notification)))
}

pub(in crate::overlay) fn view(notification: &Notification) -> Element<'static, Message> {
    widget::container(widget::icon::icon(handle(notification)).size(SIZE))
        .center_x(Length::Fixed(f32::from(SIZE)))
        .center_y(Length::Fixed(f32::from(SIZE)))
        .into()
}

fn handle(notification: &Notification) -> widget::icon::Handle {
    static CACHE: OnceLock<Mutex<HashMap<IconKey, widget::icon::Handle>>> = OnceLock::new();
    let key = IconKey {
        app: known_app(notification),
        app_icon: notification.app_icon.clone(),
        desktop_entry: notification.desktop_entry.clone(),
        app_name: notification.app_name.clone(),
    };
    let mut cache = CACHE.get_or_init(Mutex::default).lock().unwrap();
    if let Some(handle) = cache.get(&key) {
        return handle.clone();
    }
    let handle = resolve(&key);
    // Notification sources can contain conversation names. Bound the cache so
    // long-running sessions do not retain an unlimited number of old names.
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, handle.clone());
    handle
}

fn resolve(key: &IconKey) -> widget::icon::Handle {
    if key.app == Some(App::Instagram) {
        return widget::icon::from_svg_bytes(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/instagram.svg"
        )));
    }
    if let Some(app) = key.app
        && let Some(handle) = app.icon_names().iter().find_map(|name| theme_icon(name))
    {
        return handle;
    }
    if let Some(handle) = key.app_icon.as_deref().and_then(supplied_icon) {
        return handle;
    }
    if let Some(handle) = key.desktop_entry.as_deref().and_then(desktop_icon) {
        return handle;
    }
    if let Some(handle) =
        theme_icon(&key.app_name).or_else(|| theme_icon(&key.app_name.to_ascii_lowercase()))
    {
        return handle;
    }
    // Unknown sources still get a notification icon instead of a colored dot.
    let mut handle = widget::icon::from_svg_bytes(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/icons/notification-bell-filled-symbolic.svg"
    )));
    handle.symbolic = true;
    handle
}

fn theme_icon(name: &str) -> Option<widget::icon::Handle> {
    if !valid_icon_name(name) {
        return None;
    }
    widget::icon::from_name(name)
        .size(SIZE)
        .fallback(None)
        .path()
        .map(widget::icon::from_path)
}

fn valid_icon_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn supplied_icon(value: &str) -> Option<widget::icon::Handle> {
    let value = value.trim();
    let path = if value.starts_with("file://") {
        Some(reqwest::Url::parse(value).ok()?.to_file_path().ok()?)
    } else if Path::new(value).is_absolute() {
        Some(PathBuf::from(value))
    } else {
        None
    };
    if let Some(path) = path {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        return (matches!(extension.as_str(), "svg" | "png" | "jpg" | "jpeg") && path.is_file())
            .then(|| {
                if extension == "svg" {
                    widget::icon::Handle {
                        symbolic: path
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.ends_with("-symbolic")),
                        data: widget::icon::Data::Svg(widget::svg::Handle::from_path(path)),
                    }
                } else {
                    widget::icon::from_path(path)
                }
            });
    }
    // No remote image fetches; only local images and installed theme icons.
    theme_icon(value)
}

fn desktop_icon(identity: &str) -> Option<widget::icon::Handle> {
    let id = identity.trim().trim_end_matches(".desktop");
    if !valid_icon_name(id) {
        return None;
    }
    theme_icon(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(app_name: &str, summary: &str, desktop_entry: Option<&str>) -> Notification {
        serde_json::from_value(serde_json::json!({
            "app_name": app_name, "summary": summary, "body": "", "timestamp": 0,
            "desktop_entry": desktop_entry,
        }))
        .unwrap()
    }

    #[test]
    fn real_app_identity_takes_priority_over_conversation_names() {
        let discord = notification(
            "Instagram",
            "A message",
            Some("com.discordapp.Discord.desktop"),
        );
        assert_eq!(known_app(&discord), Some(App::Discord));
        let files = notification("", "Copy complete", Some("com.system76.CosmicFiles"));
        assert_eq!(known_app(&files), Some(App::Files));
        let web_instagram = notification("", "Instagram", Some("app.zen_browser.zen"));
        assert_eq!(known_app(&web_instagram), Some(App::Instagram));
        assert_eq!(
            known_app(&notification("Unrelated", "Discord update", None)),
            None
        );
    }

    #[test]
    fn supplied_icons_do_not_fetch_remote_urls_or_resolve_relative_paths() {
        for value in [
            "https://example.com/icon.png",
            "http://example.com/icon.svg",
            "../icon.svg",
            "file://example.com/icon.png",
        ] {
            assert!(supplied_icon(value).is_none(), "{value}");
        }
        assert!(desktop_icon("../outside").is_none());
    }

    #[test]
    #[ignore = "requires locally installed desktop icons"]
    fn resolves_installed_notification_icons() {
        for app in [App::Discord, App::Files, App::Firmware, App::Store] {
            let path = app.icon_names().iter().find_map(|name| {
                widget::icon::from_name(*name)
                    .size(SIZE)
                    .fallback(None)
                    .path()
            });
            println!("{app:?}: {path:?}");
            assert!(path.is_some(), "missing icon for {app:?}");
        }
    }
}
