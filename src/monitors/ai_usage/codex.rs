// SPDX-License-Identifier: MPL-2.0

//! Read the account quota snapshots that Codex records after server responses.
//!
//! These percentages come from `token_count.rate_limits`, not token estimates.
//! Session files stay local and read-only; credentials and conversation content
//! are never sent anywhere. Only a bounded tail of recently modified files is
//! read, and unchanged files are skipped. An old snapshot remains explicitly
//! stale, including after its reported reset time, until Codex records an update.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::{UsageSnapshot, UsageStatus, UsageWindow};
use serde::Deserialize;

const POLL_INTERVAL: Duration = Duration::from_secs(30);
const STALE_AFTER_SECS: i64 = 15 * 60;
const MAX_SESSION_FILES: usize = 24;
const MAX_TAIL_BYTES: usize = 512 * 1024;
const MAX_DIRECTORY_DEPTH: usize = 4;

/// The worker does all filesystem work away from the UI. Dropping the last
/// monitor closes its channel and stops the worker; disabling pauses all reads.
#[derive(Clone)]
pub struct CodexUsageMonitor {
    snapshot: Arc<Mutex<UsageSnapshot>>,
    enabled: Arc<AtomicBool>,
    wake: Sender<()>,
}

impl CodexUsageMonitor {
    pub fn new(enabled: bool) -> Self {
        let snapshot = Arc::new(Mutex::new(UsageSnapshot {
            status: if enabled {
                UsageStatus::Loading
            } else {
                UsageStatus::Unavailable
            },
            ..UsageSnapshot::default()
        }));
        let enabled = Arc::new(AtomicBool::new(enabled));
        let (wake, receiver) = channel();
        let worker_snapshot = snapshot.clone();
        let worker_enabled = enabled.clone();
        let sessions = codex_home().map(|home| home.join("sessions"));
        let worker = std::thread::Builder::new()
            .name("codex-usage".into())
            .spawn(move || {
                let mut reader = SessionReader::default();
                loop {
                    if worker_enabled.load(Ordering::Relaxed) {
                        let value = sessions
                            .as_ref()
                            .map_or_else(UsageSnapshot::default, |path| {
                                reader.refresh(path, chrono::Utc::now().timestamp())
                            });
                        if let Ok(mut snapshot) = worker_snapshot.lock() {
                            *snapshot = value;
                        }
                    }
                    if worker_enabled.load(Ordering::Relaxed) {
                        if matches!(
                            receiver.recv_timeout(POLL_INTERVAL),
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
                        ) {
                            break;
                        }
                    } else if receiver.recv().is_err() {
                        break;
                    }
                }
            });
        if worker.is_err() {
            if let Ok(mut value) = snapshot.lock() {
                value.status = UsageStatus::Unavailable;
            }
            log::warn!("Unable to start Codex usage monitor");
        }
        Self {
            snapshot,
            enabled,
            wake,
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        if self.enabled.swap(enabled, Ordering::Relaxed) != enabled {
            if enabled {
                if let Ok(mut snapshot) = self.snapshot.lock() {
                    if snapshot.windows.is_empty() {
                        snapshot.status = UsageStatus::Loading;
                    }
                }
            }
            let _ = self.wake.send(());
        }
    }

    pub fn snapshot(&self) -> UsageSnapshot {
        self.snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    }
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
}

#[derive(Debug, Deserialize)]
struct SessionEvent {
    timestamp: String,
    #[serde(rename = "type")]
    event_type: String,
    payload: TokenCount,
}

#[derive(Debug, Deserialize)]
struct TokenCount {
    #[serde(rename = "type")]
    event_type: String,
    rate_limits: Option<RateLimits>,
}

#[derive(Debug, Clone, Deserialize)]
struct RateLimits {
    // Older Codex versions omitted the ID on the general account limit.
    #[serde(default = "default_limit_id")]
    limit_id: String,
    limit_name: Option<String>,
    primary: Option<RateWindow>,
    secondary: Option<RateWindow>,
}

fn default_limit_id() -> String {
    "codex".into()
}

#[derive(Debug, Clone, Deserialize)]
struct RateWindow {
    used_percent: f64,
    window_minutes: Option<i64>,
    resets_at: Option<i64>,
}

impl RateWindow {
    fn is_valid(&self) -> bool {
        self.used_percent.is_finite()
            && (0.0..=100.0).contains(&self.used_percent)
            && self.window_minutes.is_none_or(|minutes| minutes > 0)
            && self.resets_at.is_none_or(|timestamp| timestamp > 0)
    }
}

#[derive(Debug, Clone)]
struct QuotaRecord {
    updated_at: i64,
    recorded_at_millis: i64,
    limits: RateLimits,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    length: u64,
    modified: SystemTime,
}

#[derive(Default)]
struct SessionReader {
    files: HashMap<PathBuf, FileStamp>,
    latest: BTreeMap<String, QuotaRecord>,
}

impl SessionReader {
    fn refresh(&mut self, sessions: &Path, now: i64) -> UsageSnapshot {
        let mut files = Vec::new();
        collect_session_files(sessions, 0, &mut files);
        files.sort_unstable_by(|a, b| b.1.modified.cmp(&a.1.modified).then(a.0.cmp(&b.0)));
        files.truncate(MAX_SESSION_FILES);
        let selected: HashSet<_> = files.iter().map(|(path, _)| path.clone()).collect();
        self.files.retain(|path, _| selected.contains(path));

        for (path, stamp) in files {
            if self.files.get(&path) == Some(&stamp) {
                continue;
            }
            if let Ok(tail) = read_tail(&path, stamp.length) {
                for record in records_from_tail(&tail) {
                    // Do not let an invalid future-dated record hide real data.
                    if record.updated_at > now + 5 * 60 {
                        continue;
                    }
                    merge_record(&mut self.latest, record);
                }
                self.files.insert(path, stamp);
            }
        }
        snapshot_from_records(self.latest.values(), now)
    }
}

fn collect_session_files(path: &Path, depth: usize, files: &mut Vec<(PathBuf, FileStamp)>) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && depth < MAX_DIRECTORY_DEPTH {
            collect_session_files(&entry.path(), depth + 1, files);
        } else if kind.is_file() && entry.path().extension().is_some_and(|ext| ext == "jsonl") {
            if let Ok(metadata) = entry.metadata() {
                if let Ok(modified) = metadata.modified() {
                    files.push((
                        entry.path(),
                        FileStamp {
                            length: metadata.len(),
                            modified,
                        },
                    ));
                }
            }
        }
    }
}

fn read_tail(path: &Path, length: u64) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(
        length.saturating_sub(MAX_TAIL_BYTES as u64),
    ))?;
    let mut bytes = Vec::with_capacity(MAX_TAIL_BYTES.min(length as usize));
    file.take(MAX_TAIL_BYTES as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn records_from_tail(bytes: &[u8]) -> Vec<QuotaRecord> {
    const TOKEN_COUNT: &[u8] = b"\"token_count\"";
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| {
            line.windows(TOKEN_COUNT.len())
                .any(|part| part == TOKEN_COUNT)
        })
        .filter_map(|line| {
            let event: SessionEvent = serde_json::from_slice(line).ok()?;
            if event.event_type != "event_msg" || event.payload.event_type != "token_count" {
                return None;
            }
            let mut limits = event.payload.rate_limits?;
            if limits.limit_id.trim().is_empty() {
                return None;
            }
            limits.primary = limits.primary.filter(RateWindow::is_valid);
            limits.secondary = limits.secondary.filter(RateWindow::is_valid);
            if limits.primary.is_none() && limits.secondary.is_none() {
                return None;
            }
            let timestamp = chrono::DateTime::parse_from_rfc3339(&event.timestamp).ok()?;
            Some(QuotaRecord {
                updated_at: timestamp.timestamp(),
                recorded_at_millis: timestamp.timestamp_millis(),
                limits,
            })
        })
        .collect()
}

fn merge_record(records: &mut BTreeMap<String, QuotaRecord>, record: QuotaRecord) {
    let id = record.limits.limit_id.clone();
    if records
        .get(&id)
        .is_none_or(|previous| previous.recorded_at_millis <= record.recorded_at_millis)
    {
        records.insert(id, record);
    }
}

fn window_label(minutes: Option<i64>, fallback: &str) -> String {
    match minutes {
        Some(10080) => "Weekly".into(),
        Some(1440) => "Daily".into(),
        Some(60) => "1 hour".into(),
        Some(minutes) if minutes % 1440 == 0 => format!("{} days", minutes / 1440),
        Some(minutes) if minutes % 60 == 0 => format!("{} hours", minutes / 60),
        Some(1) => "1 minute".into(),
        Some(minutes) => format!("{minutes} minutes"),
        None => fallback.into(),
    }
}

fn snapshot_from_records<'a>(
    records: impl Iterator<Item = &'a QuotaRecord>,
    now: i64,
) -> UsageSnapshot {
    let mut records: Vec<_> = records.collect();
    // The general quota comes first, followed by named model/product buckets.
    records.sort_by(|a, b| {
        (a.limits.limit_id != "codex", &a.limits.limit_id)
            .cmp(&(b.limits.limit_id != "codex", &b.limits.limit_id))
    });
    let mut snapshot = UsageSnapshot::default();
    for record in records {
        for (window, fallback) in [
            (record.limits.primary.as_ref(), "Primary"),
            (record.limits.secondary.as_ref(), "Secondary"),
        ] {
            let Some(window) = window else { continue };
            let mut label = window_label(window.window_minutes, fallback);
            if record.limits.limit_id != "codex" {
                let name = record
                    .limits
                    .limit_name
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or(&record.limits.limit_id);
                label = format!("{name} · {label}");
            }
            let is_stale = now.saturating_sub(record.updated_at) > STALE_AFTER_SECS
                || window.resets_at.is_some_and(|reset| reset <= now);
            snapshot.windows.push(UsageWindow {
                limit_id: record.limits.limit_id.clone(),
                label,
                remaining_percent: (100.0 - window.used_percent) as f32,
                resets_at: window.resets_at,
                updated_at: record.updated_at,
                is_stale,
            });
            snapshot.updated_at = Some(snapshot.updated_at.map_or(record.updated_at, |previous| {
                previous.max(record.updated_at)
            }));
        }
    }
    if !snapshot.windows.is_empty() {
        snapshot.status = if snapshot.windows.iter().any(|window| window.is_stale) {
            UsageStatus::Stale
        } else {
            UsageStatus::Available
        };
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use std::sync::atomic::AtomicU64;

    const NOW: i64 = 1790255465;

    fn event(timestamp: i64, limits: serde_json::Value) -> Vec<u8> {
        let value = json!({
            "timestamp": chrono::DateTime::from_timestamp(timestamp, 0).unwrap().to_rfc3339(),
            "type": "event_msg",
            "payload": { "type": "token_count", "rate_limits": limits }
        });
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        bytes
    }

    fn weekly(used: f64) -> serde_json::Value {
        json!({
            "limit_id": "codex", "limit_name": null,
            "primary": { "used_percent": used, "window_minutes": 10080, "resets_at": NOW + 86400 },
            "secondary": null,
            "credits": { "has_credits": false, "unlimited": false, "balance": "0" },
            "plan_type": "pro"
        })
    }

    #[test]
    fn real_weekly_primary_is_seventeen_percent_remaining() {
        let bytes = event(NOW, weekly(83.0));
        let records = records_from_tail(&bytes);
        let snapshot = snapshot_from_records(records.iter(), NOW);
        assert_eq!(snapshot.status, UsageStatus::Available);
        assert_eq!(snapshot.updated_at, Some(NOW));
        assert_eq!(snapshot.windows.len(), 1);
        assert_eq!(snapshot.windows[0].label, "Weekly");
        assert_eq!(snapshot.windows[0].remaining_percent, 17.0);
        assert_eq!(snapshot.windows[0].resets_at, Some(NOW + 86400));
    }

    #[test]
    fn separate_buckets_keep_their_identity_and_latest_timestamp() {
        let mut tail = event(NOW, weekly(83.0));
        let mut extra = weekly(25.5);
        extra["limit_id"] = json!("codex_model_specific");
        extra["limit_name"] = json!("Special model");
        extra["primary"]["window_minutes"] = json!(300);
        tail.extend(event(NOW + 10, extra));
        // Log order is not assumed to be chronological across session files.
        tail.extend(event(NOW - 5, weekly(12.0)));
        let mut latest = BTreeMap::new();
        for record in records_from_tail(&tail) {
            merge_record(&mut latest, record);
        }
        let snapshot = snapshot_from_records(latest.values(), NOW + 10);
        assert_eq!(snapshot.windows.len(), 2);
        assert_eq!(snapshot.windows[0].limit_id, "codex");
        assert_eq!(snapshot.windows[0].remaining_percent, 17.0);
        assert_eq!(snapshot.windows[1].limit_id, "codex_model_specific");
        assert_eq!(snapshot.windows[1].label, "Special model · 5 hours");
        assert_eq!(snapshot.windows[1].remaining_percent, 74.5);
    }

    #[test]
    fn subsecond_response_order_is_preserved_across_sessions() {
        let mut newer: serde_json::Value =
            serde_json::from_slice(&event(NOW, weekly(83.0))).unwrap();
        newer["timestamp"] = json!("2026-09-24T12:00:00.900Z");
        let mut older: serde_json::Value =
            serde_json::from_slice(&event(NOW, weekly(70.0))).unwrap();
        older["timestamp"] = json!("2026-09-24T12:00:00.100Z");
        let mut latest = BTreeMap::new();
        for value in [newer, older] {
            for record in records_from_tail(&serde_json::to_vec(&value).unwrap()) {
                merge_record(&mut latest, record);
            }
        }
        let snapshot = snapshot_from_records(latest.values(), NOW);
        assert_eq!(snapshot.windows[0].remaining_percent, 17.0);
    }

    #[test]
    fn specialized_bucket_is_not_substituted_for_general_quota() {
        let mut limits = weekly(95.0);
        limits["limit_id"] = json!("some_other_bucket");
        let records = records_from_tail(&event(NOW, limits));
        let snapshot = snapshot_from_records(records.iter(), NOW);
        assert_eq!(snapshot.windows[0].limit_id, "some_other_bucket");
        assert_eq!(snapshot.windows[0].label, "some_other_bucket · Weekly");
    }

    #[test]
    fn malformed_and_incomplete_tail_preserves_latest_valid_record() {
        let mut bytes = b"truncated previous entry\n{broken json}\n".to_vec();
        bytes.extend(event(NOW, weekly(83.0)));
        bytes.extend(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\"");
        let records = records_from_tail(&bytes);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].updated_at, NOW);
        assert_eq!(
            records[0].limits.primary.as_ref().unwrap().used_percent,
            83.0
        );
    }

    #[test]
    fn old_or_expired_snapshot_stays_stale_without_inventing_refill() {
        let records = records_from_tail(&event(NOW, weekly(83.0)));
        let old = snapshot_from_records(records.iter(), NOW + STALE_AFTER_SECS + 1);
        assert_eq!(old.status, UsageStatus::Stale);
        assert!(old.windows[0].is_stale);
        assert_eq!(old.windows[0].remaining_percent, 17.0);
        let expired = snapshot_from_records(records.iter(), NOW + 86400);
        assert!(expired.windows[0].is_stale);
        assert_eq!(expired.windows[0].remaining_percent, 17.0);

        let mut recently_reset = weekly(83.0);
        recently_reset["primary"]["resets_at"] = json!(NOW - 1);
        let records = records_from_tail(&event(NOW, recently_reset));
        assert_eq!(
            snapshot_from_records(records.iter(), NOW).status,
            UsageStatus::Stale
        );
    }

    #[test]
    fn invalid_percentage_or_unrelated_event_cannot_become_quota() {
        assert!(records_from_tail(&event(NOW, weekly(101.0))).is_empty());
        assert!(records_from_tail(&event(NOW, weekly(-1.0))).is_empty());
        let unrelated = String::from_utf8(event(NOW, weekly(83.0)))
            .unwrap()
            .replace("event_msg", "response_item");
        assert!(records_from_tail(unrelated.as_bytes()).is_empty());
        assert!(records_from_tail(&event(NOW, serde_json::Value::Null)).is_empty());
    }

    #[test]
    fn old_format_supports_independent_five_hour_and_weekly_windows() {
        let mut limits = weekly(20.0);
        limits.as_object_mut().unwrap().remove("limit_id");
        limits["secondary"] = limits["primary"].clone();
        limits["primary"]["window_minutes"] = json!(300);
        limits["primary"]["used_percent"] = json!(40.0);
        let records = records_from_tail(&event(NOW, limits));
        let snapshot = snapshot_from_records(records.iter(), NOW);
        assert_eq!(snapshot.windows.len(), 2);
        assert_eq!(snapshot.windows[0].label, "5 hours");
        assert_eq!(snapshot.windows[0].remaining_percent, 60.0);
        assert_eq!(snapshot.windows[1].label, "Weekly");
        assert_eq!(snapshot.windows[1].remaining_percent, 80.0);
    }

    #[test]
    fn bounded_file_reads_keep_last_known_quota_until_new_response() {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        struct TempDirectory(PathBuf);
        impl Drop for TempDirectory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let directory = TempDirectory(std::env::temp_dir().join(format!(
            "cosmic-widget-codex-usage-test-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        )));
        fs::create_dir_all(&directory.0).unwrap();
        let path = directory.0.join("rollout.jsonl");
        fs::write(&path, event(NOW, weekly(83.0))).unwrap();
        let mut reader = SessionReader::default();
        assert_eq!(
            reader.refresh(&directory.0, NOW).windows[0].remaining_percent,
            17.0
        );

        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&vec![b'x'; MAX_TAIL_BYTES + 64]).unwrap();
        file.write_all(b"\n").unwrap();
        let tail = read_tail(&path, fs::metadata(&path).unwrap().len()).unwrap();
        assert_eq!(tail.len(), MAX_TAIL_BYTES);
        assert!(records_from_tail(&tail).is_empty());
        let stale = reader.refresh(&directory.0, NOW + STALE_AFTER_SECS + 1);
        assert_eq!(stale.windows[0].remaining_percent, 17.0);
        assert_eq!(stale.status, UsageStatus::Stale);

        file.write_all(&event(NOW - 1, weekly(70.0))).unwrap();
        assert_eq!(
            reader.refresh(&directory.0, NOW).windows[0].remaining_percent,
            17.0
        );
        file.write_all(&event(NOW + 1, weekly(84.0))).unwrap();
        let fresh = reader.refresh(&directory.0, NOW + 1);
        assert_eq!(fresh.windows[0].remaining_percent, 16.0);
        assert_eq!(fresh.status, UsageStatus::Available);
        assert_eq!(reader.refresh(&directory.0, NOW + 1), fresh);
    }

    #[test]
    #[ignore = "Reads local Codex quota metadata; run explicitly for a local smoke check"]
    fn local_codex_usage_snapshot() {
        let path = codex_home().expect("Codex home directory").join("sessions");
        let snapshot = SessionReader::default().refresh(&path, chrono::Utc::now().timestamp());
        // Print only sanitized quota fields, never paths, prompts, or auth data.
        println!("{snapshot:?}");
        assert!(
            !snapshot.windows.is_empty(),
            "No local Codex quota snapshot found"
        );
    }
}
