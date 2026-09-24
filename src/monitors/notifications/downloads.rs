// SPDX-License-Identifier: MPL-2.0

//! Resolve safe local actions for completed-download notifications.

use super::Notification;
use serde::Deserialize;
use serde::de::IgnoredAny;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Deserialize)]
struct TorrentMetadata {
    info: TorrentInfo,
}

#[derive(Deserialize)]
struct TorrentInfo {
    name: String,
    #[serde(default)]
    files: Vec<TorrentFile>,
    #[serde(rename = "file tree")]
    file_tree: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct TorrentFile {
    #[serde(rename = "path")]
    _path: Vec<String>,
}

#[derive(Deserialize)]
struct FastResume {
    #[serde(rename = "qBt-savePath")]
    qbt_save_path: Option<String>,
    #[serde(rename = "qBt-name")]
    qbt_name: Option<String>,
    #[serde(rename = "qBt-contentLayout")]
    qbt_content_layout: Option<String>,
    save_path: Option<String>,
}

pub(super) fn resolve_open_folders(notifications: &mut [Notification]) {
    for notification in notifications {
        if let Some(target) = resolve_qbittorrent_target(notification) {
            notification.open_folder = Some(target);
        }
    }
}

fn resolve_qbittorrent_target(notification: &Notification) -> Option<PathBuf> {
    if !notification.app_name.eq_ignore_ascii_case("qBittorrent")
        || !notification
            .summary
            .eq_ignore_ascii_case("Download completed")
    {
        return None;
    }

    qbittorrent_backup_directories()
        .into_iter()
        .find_map(|directory| find_qbittorrent_content_target(&directory, &notification.body))
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

fn find_qbittorrent_content_target(backup_directory: &Path, body: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(backup_directory).ok()?;
    for torrent_path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
        if torrent_path
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("torrent")
        {
            continue;
        }
        let Ok(torrent_bytes) = fs::read(&torrent_path) else {
            continue;
        };
        let Some(torrent) = torrent_metadata(&torrent_bytes) else {
            continue;
        };
        let resume_path = torrent_path.with_extension("fastresume");
        let Ok(resume_bytes) = fs::read(resume_path) else {
            continue;
        };
        let Some(resume) = fastresume(&resume_bytes) else {
            continue;
        };
        let effective_name = resume.effective_name(&torrent.info.name);
        if !body.contains(&torrent.info.name)
            && !effective_name.is_some_and(|name| body.contains(name))
        {
            continue;
        }

        let Some(save_path) = resume.save_path() else {
            continue;
        };
        if !save_path.is_dir() {
            continue;
        }

        if torrent.info.files.is_empty()
            && let Some(name) = effective_name.and_then(safe_path_component)
        {
            let content_file = save_path.join(name);
            if content_file.is_file() {
                return Some(content_file);
            }
        }

        if torrent.info.is_multi_file()
            && resume.uses_content_directory()
            && let Some(name) = effective_name.and_then(safe_path_component)
        {
            let content_directory = save_path.join(name);
            if content_directory.is_dir() {
                return Some(content_directory);
            }
        }

        return Some(save_path);
    }
    None
}

#[cfg(test)]
fn torrent_name(bytes: &[u8]) -> Option<String> {
    torrent_metadata(bytes)
        .map(|metadata| metadata.info.name)
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
fn fastresume_save_path(bytes: &[u8]) -> Option<PathBuf> {
    fastresume(bytes)?.save_path()
}

fn torrent_metadata(bytes: &[u8]) -> Option<TorrentMetadata> {
    serde_bencode::from_bytes(bytes).ok()
}

fn fastresume(bytes: &[u8]) -> Option<FastResume> {
    serde_bencode::from_bytes(bytes).ok()
}

impl FastResume {
    fn save_path(&self) -> Option<PathBuf> {
        self.qbt_save_path
            .as_deref()
            .or(self.save_path.as_deref())
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
    }

    fn effective_name<'a>(&'a self, torrent_name: &'a str) -> Option<&'a str> {
        self.qbt_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or((!torrent_name.is_empty()).then_some(torrent_name))
    }

    fn uses_content_directory(&self) -> bool {
        !self
            .qbt_content_layout
            .as_deref()
            .is_some_and(|layout| layout.eq_ignore_ascii_case("NoSubfolder"))
    }
}

impl TorrentInfo {
    fn is_multi_file(&self) -> bool {
        !self.files.is_empty() || self.file_tree.is_some()
    }
}

fn safe_path_component(name: &str) -> Option<&str> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Some(name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fastresume, fastresume_save_path, find_qbittorrent_content_target, safe_path_component,
        torrent_metadata, torrent_name,
    };
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn parses_qbittorrent_torrent_name_and_save_path() {
        let torrent = b"d4:infod4:name9:movie.mkvee";
        let resume = b"d8:qBt-name0:12:qBt-savePath14:/mnt/Downloads9:save_path7:/ignoree";

        assert_eq!(torrent_name(torrent).as_deref(), Some("movie.mkv"));
        assert_eq!(
            fastresume_save_path(resume),
            Some(PathBuf::from("/mnt/Downloads"))
        );
        assert!(
            torrent_metadata(b"d4:infod9:file treede4:name6:Folderee")
                .unwrap()
                .info
                .is_multi_file()
        );
        assert!(
            !fastresume(b"d17:qBt-contentLayout11:NoSubfoldere")
                .unwrap()
                .uses_content_directory()
        );
    }

    #[test]
    fn opens_a_single_file_torrents_content_file() {
        let root = std::env::temp_dir().join(format!(
            "cosmic-widget-qbittorrent-test-{}",
            std::process::id()
        ));
        let backup = root.join("BT_backup");
        let save_path = root.join("Downloads");
        fs::create_dir_all(&backup).unwrap();
        fs::create_dir_all(&save_path).unwrap();
        let content_path = save_path.join("movie.mkv");
        fs::write(&content_path, b"test").unwrap();
        fs::write(backup.join("test.torrent"), b"d4:infod4:name9:movie.mkvee").unwrap();
        let path = save_path.to_string_lossy();
        fs::write(
            backup.join("test.fastresume"),
            format!("d12:qBt-savePath{}:{}e", path.len(), path),
        )
        .unwrap();

        assert_eq!(
            find_qbittorrent_content_target(&backup, "'movie.mkv' has finished downloading."),
            Some(content_path)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opens_a_multi_file_torrents_content_directory() {
        let root = std::env::temp_dir().join(format!(
            "cosmic-widget-qbittorrent-folder-test-{}",
            std::process::id()
        ));
        let backup = root.join("BT_backup");
        let save_path = root.join("Downloads");
        let content_path = save_path.join("Example Collection");
        fs::create_dir_all(&backup).unwrap();
        fs::create_dir_all(&content_path).unwrap();
        fs::write(
            backup.join("test.torrent"),
            b"d4:infod5:filesld6:lengthi1e4:pathl8:file.txteee4:name20:Original Name Folderee",
        )
        .unwrap();
        let path = save_path.to_string_lossy();
        fs::write(
            backup.join("test.fastresume"),
            format!(
                "d17:qBt-contentLayout9:Subfolder8:qBt-name18:Example Collection12:qBt-savePath{}:{}e",
                path.len(),
                path
            ),
        )
        .unwrap();

        assert_eq!(
            find_qbittorrent_content_target(
                &backup,
                "'Example Collection' has finished downloading."
            ),
            Some(content_path)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_torrent_names_that_can_escape_the_save_directory() {
        assert_eq!(safe_path_component("Folder"), Some("Folder"));
        assert_eq!(safe_path_component("../Folder"), None);
        assert_eq!(safe_path_component("Folder/Child"), None);
        assert_eq!(safe_path_component("/tmp/Folder"), None);
    }
}
