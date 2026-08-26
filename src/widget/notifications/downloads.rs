// SPDX-License-Identifier: MPL-2.0

//! Resolve safe local actions for completed-download notifications.

use super::Notification;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct TorrentMetadata {
    info: TorrentInfo,
}

#[derive(Deserialize)]
struct TorrentInfo {
    name: String,
}

#[derive(Deserialize)]
struct FastResume {
    #[serde(rename = "qBt-savePath")]
    qbt_save_path: Option<String>,
    save_path: Option<String>,
}

pub(super) fn resolve_open_folders(notifications: &mut [Notification]) {
    for notification in notifications {
        if notification.open_folder.is_none() {
            notification.open_folder = resolve_qbittorrent_folder(notification);
        }
    }
}

fn resolve_qbittorrent_folder(notification: &Notification) -> Option<PathBuf> {
    if !notification.app_name.eq_ignore_ascii_case("qBittorrent")
        || !notification
            .summary
            .eq_ignore_ascii_case("Download completed")
    {
        return None;
    }

    qbittorrent_backup_directories()
        .into_iter()
        .find_map(|directory| find_qbittorrent_save_path(&directory, &notification.body))
}

fn qbittorrent_backup_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(data_dir) = dirs::data_dir() {
        directories.push(data_dir.join("qBittorrent/BT_backup"));
    }
    if let Some(home) = dirs::home_dir() {
        directories
            .push(home.join(".var/app/org.qbittorrent.qBittorrent/data/qBittorrent/BT_backup"));
    }
    directories
}

fn find_qbittorrent_save_path(backup_directory: &Path, body: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(backup_directory).ok()?;
    for torrent_path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
        if torrent_path
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("torrent")
        {
            continue;
        }
        let name = torrent_name(&fs::read(&torrent_path).ok()?)?;
        if !body.contains(&name) {
            continue;
        }

        let resume_path = torrent_path.with_extension("fastresume");
        let save_path = fastresume_save_path(&fs::read(resume_path).ok()?)?;
        if save_path.is_dir() {
            return Some(save_path);
        }
    }
    None
}

fn torrent_name(bytes: &[u8]) -> Option<String> {
    serde_bencode::from_bytes::<TorrentMetadata>(bytes)
        .ok()
        .map(|metadata| metadata.info.name)
        .filter(|name| !name.is_empty())
}

fn fastresume_save_path(bytes: &[u8]) -> Option<PathBuf> {
    let resume = serde_bencode::from_bytes::<FastResume>(bytes).ok()?;
    resume
        .qbt_save_path
        .or(resume.save_path)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::{fastresume_save_path, find_qbittorrent_save_path, torrent_name};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn parses_qbittorrent_torrent_name_and_save_path() {
        let torrent = b"d4:infod4:name9:movie.mkvee";
        let resume = b"d12:qBt-savePath14:/mnt/Downloads9:save_path7:/ignoree";

        assert_eq!(torrent_name(torrent).as_deref(), Some("movie.mkv"));
        assert_eq!(
            fastresume_save_path(resume),
            Some(PathBuf::from("/mnt/Downloads"))
        );
    }

    #[test]
    fn matches_a_completion_body_to_its_existing_save_directory() {
        let root = std::env::temp_dir().join(format!(
            "cosmic-widget-qbittorrent-test-{}",
            std::process::id()
        ));
        let backup = root.join("BT_backup");
        let save_path = root.join("Downloads");
        fs::create_dir_all(&backup).unwrap();
        fs::create_dir_all(&save_path).unwrap();
        fs::write(backup.join("test.torrent"), b"d4:infod4:name9:movie.mkvee").unwrap();
        let path = save_path.to_string_lossy();
        fs::write(
            backup.join("test.fastresume"),
            format!("d12:qBt-savePath{}:{}e", path.len(), path),
        )
        .unwrap();

        assert_eq!(
            find_qbittorrent_save_path(&backup, "'movie.mkv' has finished downloading."),
            Some(save_path)
        );
        fs::remove_dir_all(root).unwrap();
    }
}
