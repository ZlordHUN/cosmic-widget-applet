// SPDX-License-Identifier: MPL-2.0

//! Durable, credential-bound quota snapshots. Access tokens are never persisted.

use super::{UsageSnapshot, UsageStatus};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) const CACHE_VERSION: u32 = 1;
const MAX_CACHE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CacheRecord {
    pub(super) version: u32,
    pub(super) credential_key: String,
    pub(super) snapshot: UsageSnapshot,
    pub(super) next_request_at: i64,
    pub(super) failures: u32,
}

pub(super) fn credential_key(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|path| path.join("cosmic-widget-applet/claude-usage.json"))
}

/// Restore only a matching credential's bounded, validated report. Timestamps
/// and retry deadlines remain unchanged; the caller decides how to show age.
pub(super) fn load(path: &Path, key: &str, now: i64) -> Option<CacheRecord> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CACHE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return None;
    }
    let record: CacheRecord = serde_json::from_slice(&bytes).ok()?;
    valid(&record, key, now).then_some(record)
}

fn valid(record: &CacheRecord, key: &str, now: i64) -> bool {
    if record.version != CACHE_VERSION
        || record.credential_key != key
        || key.len() != 64
        || !key.bytes().all(|byte| byte.is_ascii_hexdigit())
        || record.next_request_at < 0
        || record.snapshot.windows.len() > 6
    {
        return false;
    }
    let valid_timestamp = |timestamp: i64| timestamp > 0 && timestamp <= now;
    if record
        .snapshot
        .updated_at
        .is_some_and(|timestamp| !valid_timestamp(timestamp))
    {
        return false;
    }
    if record.snapshot.windows.is_empty() {
        return matches!(
            record.snapshot.status,
            UsageStatus::Loading | UsageStatus::SignInRequired | UsageStatus::Unavailable
        );
    }
    if !matches!(
        record.snapshot.status,
        UsageStatus::Available | UsageStatus::Stale
    ) || record.snapshot.updated_at.is_none()
    {
        return false;
    }
    let mut seen = std::collections::HashSet::new();
    record.snapshot.windows.iter().all(|window| {
        let label = match window.limit_id.as_str() {
            "five_hour" => "5-hour",
            "seven_day" => "Weekly",
            "seven_day_sonnet" => "Sonnet · Weekly",
            "seven_day_opus" => "Opus · Weekly",
            "seven_day_cowork" => "Cowork · Weekly",
            "seven_day_oauth_apps" => "OAuth apps · Weekly",
            _ => return false,
        };
        window.label == label
            && seen.insert(window.limit_id.as_str())
            && window.remaining_percent.is_finite()
            && (0.0..=100.0).contains(&window.remaining_percent)
            && valid_timestamp(window.updated_at)
            && record.snapshot.updated_at >= Some(window.updated_at)
            && window.resets_at.is_none_or(|reset| {
                reset > 0 && chrono::DateTime::from_timestamp(reset, 0).is_some()
            })
    })
}

/// Atomically replace the report using an exclusively created, private file.
/// The temporary file is always on the same filesystem as the destination.
pub(super) fn save(path: &Path, record: &CacheRecord) -> io::Result<()> {
    let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Claude quota cache exceeds size limit",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing cache directory"))?;
    fs::create_dir_all(parent)?;
    let (temporary, mut file) = temporary_file(parent)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn temporary_file(parent: &Path) -> io::Result<(PathBuf, File)> {
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    for _ in 0..64 {
        let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".claude-usage-{}-{id}.tmp", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Unable to create unique Claude quota cache file",
    ))
}

pub(super) fn clear(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitors::ai_usage::UsageWindow;
    use std::os::unix::fs::PermissionsExt;

    const NOW: i64 = 1_800_000_000;
    const TOKEN: &str = "fake-access-token-not-for-persistence";

    struct CacheDirectory(PathBuf);

    impl CacheDirectory {
        fn new() -> Self {
            static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
            let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "cosmic-claude-cache-test-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> PathBuf {
            self.0.join("claude-usage.json")
        }
    }

    impl Drop for CacheDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn report() -> CacheRecord {
        CacheRecord {
            version: CACHE_VERSION,
            credential_key: credential_key(TOKEN),
            snapshot: UsageSnapshot {
                windows: vec![UsageWindow {
                    limit_id: "five_hour".into(),
                    label: "5-hour".into(),
                    remaining_percent: 73.5,
                    resets_at: Some(NOW + 3600),
                    updated_at: NOW - 60,
                    is_stale: true,
                }],
                updated_at: Some(NOW - 60),
                status: UsageStatus::Stale,
            },
            next_request_at: NOW + 900,
            failures: 2,
        }
    }

    #[test]
    fn reload_preserves_report_age_and_retry_deadline_without_persisting_token() {
        let directory = CacheDirectory::new();
        let record = report();
        save(&directory.path(), &record).unwrap();
        let reloaded = load(&directory.path(), &credential_key(TOKEN), NOW).unwrap();
        assert_eq!(reloaded.snapshot, record.snapshot);
        assert_eq!(reloaded.next_request_at, record.next_request_at);
        assert_eq!(reloaded.failures, record.failures);
        assert!(
            !fs::read_to_string(directory.path())
                .unwrap()
                .contains(TOKEN)
        );
        assert!(load(&directory.path(), &credential_key("other-account"), NOW).is_none());
        assert_eq!(
            credential_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn empty_auth_and_retry_snapshots_can_restore_their_deadlines() {
        let directory = CacheDirectory::new();
        for (status, deadline) in [
            (UsageStatus::SignInRequired, i64::MAX),
            (UsageStatus::Unavailable, NOW + 3600),
        ] {
            let mut record = report();
            record.snapshot = UsageSnapshot {
                status,
                ..UsageSnapshot::default()
            };
            record.next_request_at = deadline;
            save(&directory.path(), &record).unwrap();
            let loaded = load(&directory.path(), &record.credential_key, NOW).unwrap();
            assert_eq!(loaded.snapshot.status, status);
            assert!(loaded.snapshot.windows.is_empty());
            assert_eq!(loaded.next_request_at, deadline);
        }
    }

    #[test]
    fn rejects_malformed_oversized_future_and_invalid_quota_records() {
        let directory = CacheDirectory::new();
        let path = directory.path();
        fs::write(&path, b"not JSON").unwrap();
        assert!(load(&path, &credential_key(TOKEN), NOW).is_none());
        fs::write(&path, vec![b' '; MAX_CACHE_BYTES as usize + 1]).unwrap();
        assert!(load(&path, &credential_key(TOKEN), NOW).is_none());
        for invalid in 0..7 {
            let mut record = report();
            match invalid {
                0 => record.version += 1,
                1 => record.snapshot.windows[0].limit_id = "unrecognized".into(),
                2 => record.snapshot.windows[0].remaining_percent = 101.0,
                3 => record.snapshot.windows[0].updated_at = NOW + 1,
                4 => record.snapshot.updated_at = Some(0),
                5 => record
                    .snapshot
                    .windows
                    .push(record.snapshot.windows[0].clone()),
                _ => record.snapshot.windows[0].resets_at = Some(-1),
            }
            save(&path, &record).unwrap();
            assert!(load(&path, &record.credential_key, NOW).is_none());
        }
    }

    #[test]
    fn replacement_is_private_and_cleans_temporary_files() {
        let directory = CacheDirectory::new();
        let path = directory.path();
        save(&path, &report()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        save(&path, &report()).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        clear(&path);
        assert!(!path.exists());
        // Failed replacement must clean only the temporary file it created.
        fs::create_dir(&path).unwrap();
        assert!(save(&path, &report()).is_err());
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        assert!(path.is_dir());
    }
}
