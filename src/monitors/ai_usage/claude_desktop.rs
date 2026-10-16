// SPDX-License-Identifier: MPL-2.0

//! Read Claude Desktop's existing usage reports without accessing its credentials.

mod cache;

use cache::UsageCacheReader;

use super::{UsageSnapshot, UsageSource, UsageStatus, UsageWindow};
use serde::Deserialize;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_HISTORY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SAMPLES: usize = 16_384;
const FRESH_SECONDS: i64 = 20 * 60;
const MAX_AGE_SECONDS: i64 = 24 * 60 * 60;

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileStamp {
    fn read(path: &Path, limit: u64) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        if !metadata.is_file() || metadata.len() > limit {
            return None;
        }
        Some(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

// Deserialize only these two nonsecret fields, discarding unrelated app settings.
#[derive(Deserialize)]
struct DesktopConfig {
    #[serde(rename = "windowSizeWasSignedIn")]
    signed_in: bool,
    #[serde(rename = "lastKnownAccountUuid")]
    account_id: String,
}

#[derive(Deserialize)]
struct DesktopHistory {
    version: u32,
    samples: Vec<HistorySample>,
}

#[derive(Deserialize)]
struct HistorySample {
    t: i64,
    org: Option<String>,
    u: HistoryUsage,
}

#[derive(Deserialize)]
struct HistoryUsage {
    fh: Option<f64>,
    sd: Option<f64>,
}

#[derive(Clone)]
struct DesktopReport {
    organization: String,
    timestamp_ms: i64,
    five_hour: f32,
    seven_day: f32,
}

/// File metadata avoids reparsing unchanged reports; their age is still checked
/// on every read. The desktop app owns both files and all network refreshes.
pub(super) struct DesktopUsageReader {
    directory: Option<PathBuf>,
    cached_inputs: Option<(FileStamp, FileStamp)>,
    report: Option<DesktopReport>,
    usage_cache: UsageCacheReader,
    account_id: Option<String>,
    report_after_ms: Option<i64>,
}

impl Default for DesktopUsageReader {
    fn default() -> Self {
        Self::at(dirs::config_dir().map(|path| path.join("Claude")))
    }
}

impl DesktopUsageReader {
    fn at(directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            cached_inputs: None,
            report: None,
            usage_cache: UsageCacheReader::default(),
            account_id: None,
            report_after_ms: None,
        }
    }

    pub(super) fn snapshot(&mut self, now: i64) -> Option<UsageSnapshot> {
        let directory = self.directory.as_ref()?;
        let config_path = directory.join("config.json");
        let history_path = directory.join("plan-usage-history.json");
        let Some(config_stamp) = FileStamp::read(&config_path, MAX_CONFIG_BYTES) else {
            self.clear_report();
            return None;
        };
        let history_stamp = FileStamp::read(&history_path, MAX_HISTORY_BYTES);
        if let Some(history_stamp) = history_stamp
            && self.cached_inputs == Some((config_stamp, history_stamp))
        {
            return self
                .report
                .clone()
                .and_then(|report| self.present(report, now));
        }

        let config = read_bounded(&config_path, MAX_CONFIG_BYTES)
            .and_then(|bytes| serde_json::from_slice::<DesktopConfig>(&bytes).ok());
        let Some(config) = config else {
            self.clear_report();
            return None;
        };
        if !config.signed_in {
            self.clear_account(now);
            return None;
        }
        if !valid_identity(&config.account_id) {
            self.clear_report();
            return None;
        }
        if self
            .account_id
            .as_ref()
            .is_some_and(|id| id != &config.account_id)
        {
            self.report_after_ms = Some(now.saturating_mul(1000));
        }
        self.account_id = Some(config.account_id);

        let Some(history_stamp) = history_stamp else {
            self.clear_report();
            return None;
        };
        let report =
            read_bounded(&history_path, MAX_HISTORY_BYTES).and_then(|bytes| parse_report(&bytes));
        let Some(report) = report else {
            self.clear_report();
            return None;
        };
        // An atomic replacement or concurrent rewrite must be read next time,
        // rather than associated with the wrong metadata fingerprint.
        if FileStamp::read(&config_path, MAX_CONFIG_BYTES) != Some(config_stamp)
            || FileStamp::read(&history_path, MAX_HISTORY_BYTES) != Some(history_stamp)
        {
            self.clear_report();
            return None;
        }
        self.cached_inputs = Some((config_stamp, history_stamp));
        self.report = Some(report.clone());
        self.present(report, now)
    }

    fn present(&mut self, report: DesktopReport, now: i64) -> Option<UsageSnapshot> {
        if report.timestamp_ms <= 0
            || report.timestamp_ms > now.checked_mul(1000)?.checked_add(999)?
            || self
                .report_after_ms
                .is_some_and(|cutoff| report.timestamp_ms <= cutoff)
        {
            return None;
        }
        let updated_at = report.timestamp_ms / 1000;
        let age = now.checked_sub(updated_at)?;
        if !(0..=MAX_AGE_SECONDS).contains(&age) {
            return None;
        }
        if let Some(snapshot) =
            self.usage_cache
                .snapshot(self.directory.as_ref()?, &report.organization, now)
            && cache_matches_report(&snapshot, &report)
            && self.report_after_ms.is_none_or(|cutoff| {
                snapshot
                    .updated_at
                    .is_some_and(|timestamp| timestamp > cutoff / 1000)
            })
        {
            return Some(snapshot);
        }
        let stale = age > FRESH_SECONDS;
        let window = |limit_id: &str, label: &str, used_percent| UsageWindow {
            limit_id: limit_id.into(),
            label: label.into(),
            remaining_percent: 100.0 - used_percent,
            resets_at: None,
            updated_at,
            is_stale: stale,
        };
        Some(UsageSnapshot {
            windows: vec![
                window("five_hour", "5-hour", report.five_hour),
                window("seven_day", "Weekly", report.seven_day),
            ],
            updated_at: Some(updated_at),
            status: if stale {
                UsageStatus::Stale
            } else {
                UsageStatus::Available
            },
            source: Some(UsageSource::ClaudeDesktop),
        })
    }

    fn clear_report(&mut self) {
        self.cached_inputs = None;
        self.report = None;
        self.usage_cache = UsageCacheReader::default();
    }

    fn clear_account(&mut self, now: i64) {
        // Startup with absent or incomplete app state must not prevent restoring
        // a valid recent report when those files become available later.
        if self.account_id.take().is_some() {
            self.report_after_ms = Some(now.saturating_mul(1000));
        }
        self.clear_report();
    }
}

/// Select a complete HTTP report, never attach its resets to another reading.
fn cache_matches_report(snapshot: &UsageSnapshot, history: &DesktopReport) -> bool {
    let Some(timestamp) = snapshot.updated_at else {
        return false;
    };
    let windows = [
        ("five_hour", history.five_hour),
        ("seven_day", history.seven_day),
    ];
    let mut same_percentages = true;
    for (id, used) in windows {
        let Some(window) = snapshot.windows.iter().find(|window| window.limit_id == id) else {
            return false;
        };
        same_percentages &= (window.remaining_percent - (100.0 - used)).abs() < 0.001;
    }
    let history_time = history.timestamp_ms / 1000;
    // The HTTP Date has whole-second precision and can precede the history
    // write slightly. Only accept that overlap when both percentages match.
    let same_period = !snapshot
        .windows
        .iter()
        .any(|window| window.resets_at.is_some_and(|reset| reset <= history_time));
    timestamp > history_time
        || (timestamp >= history_time.saturating_sub(5) && same_percentages && same_period)
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.trim() == value
}

fn read_bounded(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > limit {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

fn parse_report(bytes: &[u8]) -> Option<DesktopReport> {
    if bytes.len() as u64 > MAX_HISTORY_BYTES {
        return None;
    }
    let history: DesktopHistory = serde_json::from_slice(bytes).ok()?;
    if history.version != 2 || history.samples.len() > MAX_SAMPLES {
        return None;
    }
    // The latest organization wins even if its newest sample is incomplete.
    // Never revive another organization's earlier report or combine buckets.
    let latest = history
        .samples
        .iter()
        .filter(|sample| sample.t > 0)
        .max_by_key(|sample| sample.t)?;
    let organization = latest.org.as_deref().filter(|org| valid_identity(org))?;
    history
        .samples
        .iter()
        .filter(|sample| sample.t > 0 && sample.org.as_deref() == Some(organization))
        .filter_map(|sample| {
            let valid = |value: Option<f64>| {
                value.filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
            };
            Some(DesktopReport {
                organization: organization.to_owned(),
                timestamp_ms: sample.t,
                five_hour: valid(sample.u.fh)? as f32,
                seven_day: valid(sample.u.sd)? as f32,
            })
        })
        .max_by_key(|report| report.timestamp_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    const NOW: i64 = 1_800_000_000;

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "cosmic-claude-desktop-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn config(&self, account: &str, signed_in: bool) {
            fs::write(self.0.join("config.json"), serde_json::to_vec(&json!({"windowSizeWasSignedIn":signed_in,"lastKnownAccountUuid":account,"oauth:tokenCacheV2":"ignored-private-data"})).unwrap()).unwrap();
        }

        fn history(&self, samples: serde_json::Value) {
            fs::write(
                self.0.join("plan-usage-history.json"),
                serde_json::to_vec(&json!({"version":2,"samples":samples})).unwrap(),
            )
            .unwrap();
        }

        fn reader(&self) -> DesktopUsageReader {
            DesktopUsageReader::at(Some(self.0.clone()))
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample(timestamp: i64, org: &str, fh: f64, sd: f64) -> serde_json::Value {
        json!({"t":timestamp * 1000,"org":org,"u":{"fh":fh,"sd":sd}})
    }

    #[test]
    fn startup_and_restart_preserve_report_age_and_unknown_resets() {
        let directory = Directory::new();
        directory.config("account-a", true);
        directory.history(json!([sample(NOW - 60, "org-a", 12.5, 37.0)]));
        let first = directory.reader().snapshot(NOW).unwrap();
        assert_eq!(first.updated_at, Some(NOW - 60));
        assert_eq!(first.source, Some(UsageSource::ClaudeDesktop));
        assert_eq!(first.windows[0].remaining_percent, 87.5);
        assert!(
            first
                .windows
                .iter()
                .all(|window| window.resets_at.is_none())
        );
        let restored = directory.reader().snapshot(NOW + FRESH_SECONDS).unwrap();
        assert_eq!(restored.updated_at, first.updated_at);
        assert_eq!(restored.status, UsageStatus::Stale);
        assert!(restored.windows.iter().all(|window| window.is_stale));
    }

    #[test]
    fn unchanged_files_still_age_and_expire() {
        let directory = Directory::new();
        directory.config("account-a", true);
        directory.history(json!([sample(NOW, "org-a", 1.0, 2.0)]));
        let mut reader = directory.reader();
        assert_eq!(reader.snapshot(NOW).unwrap().status, UsageStatus::Available);
        assert!(reader.cached_inputs.is_some());
        assert_eq!(
            reader.snapshot(NOW + FRESH_SECONDS).unwrap().status,
            UsageStatus::Available
        );
        assert_eq!(
            reader.snapshot(NOW + FRESH_SECONDS + 1).unwrap().status,
            UsageStatus::Stale
        );
        assert!(reader.snapshot(NOW + MAX_AGE_SECONDS).is_some());
        assert!(reader.snapshot(NOW + MAX_AGE_SECONDS + 1).is_none());
        assert!(directory.reader().snapshot(NOW - 1).is_none());
    }

    #[test]
    fn newest_organization_and_complete_sample_are_never_mixed() {
        let directory = Directory::new();
        directory.config("account-a", true);
        directory.history(json!([
            sample(NOW - 120, "org-a", 90.0, 80.0),
            {"t":(NOW-60)*1000,"org":"org-b","u":{"fh":10.0}},
            {"t":NOW*1000,"org":"org-b","u":{"sd":20.0}}
        ]));
        assert!(directory.reader().snapshot(NOW).is_none());
        directory.history(json!([
            sample(NOW - 120, "org-b", 3.0, 4.0),
            sample(NOW - 60, "org-a", 90.0, 80.0),
            {"t":NOW*1000,"org":"org-b","u":{"fh":10.0}}
        ]));
        let snapshot = directory.reader().snapshot(NOW).unwrap();
        assert_eq!(snapshot.updated_at, Some(NOW - 120));
        assert_eq!(snapshot.windows[0].remaining_percent, 97.0);
        assert_eq!(snapshot.windows[1].remaining_percent, 96.0);
    }

    #[test]
    fn account_change_and_logout_require_a_new_report() {
        let directory = Directory::new();
        directory.config("account-a", true);
        directory.history(json!([sample(NOW - 60, "org-a", 1.0, 2.0)]));
        let mut reader = directory.reader();
        assert!(reader.snapshot(NOW).is_some());
        directory.config("account-b", true);
        assert!(reader.snapshot(NOW + 1).is_none());
        directory.history(json!([sample(NOW + 2, "org-b", 3.0, 4.0)]));
        assert!(reader.snapshot(NOW + 2).is_some());
        directory.config("account-b", false);
        assert!(reader.snapshot(NOW + 3).is_none());
        assert!(reader.report.is_none());
        directory.config("account-b", true);
        assert!(reader.snapshot(NOW + 4).is_none());
        directory.history(json!([sample(NOW + 5, "org-b", 5.0, 6.0)]));
        assert!(reader.snapshot(NOW + 5).is_some());
    }

    #[test]
    fn absent_or_invalid_startup_state_can_restore_a_recent_report() {
        let directory = Directory::new();
        let mut reader = directory.reader();
        assert!(reader.snapshot(NOW).is_none());
        fs::write(directory.0.join("config.json"), "invalid").unwrap();
        assert!(reader.snapshot(NOW).is_none());
        directory.config("account-a", true);
        directory.history(json!([sample(NOW - 60, "org-a", 1.0, 2.0)]));
        assert!(reader.snapshot(NOW).is_some());
        fs::write(directory.0.join("config.json"), "invalid").unwrap();
        assert!(reader.snapshot(NOW).is_none());
        assert!(reader.report.is_none());
        directory.config("account-a", true);
        assert!(reader.snapshot(NOW).is_some());
        fs::remove_file(directory.0.join("config.json")).unwrap();
        assert!(reader.snapshot(NOW).is_none());
        directory.config("account-a", true);
        assert!(reader.snapshot(NOW).is_some());
        fs::write(directory.0.join("plan-usage-history.json"), "invalid").unwrap();
        assert!(reader.snapshot(NOW).is_none());
        assert!(reader.report.is_none());
        directory.history(json!([sample(NOW - 60, "org-a", 1.0, 2.0)]));
        assert!(reader.snapshot(NOW).is_some());
        fs::remove_file(directory.0.join("plan-usage-history.json")).unwrap();
        assert!(reader.snapshot(NOW).is_none());
        assert!(reader.report.is_none());
    }

    #[test]
    fn cached_reset_times_refresh_independently_and_never_override_newer_history() {
        const ORG: &str = "11111111-1111-4111-8111-111111111111";
        let directory = Directory::new();
        directory.config("account-a", true);
        directory.history(json!([sample(NOW, ORG, 1.0, 2.0)]));
        let resets = [NOW + 3600, NOW + 86400];
        cache::write_test_report(&directory.0, ORG, NOW, [1.0, 2.0], resets);
        let mut reader = directory.reader();
        let snapshot = reader.snapshot(NOW).unwrap();
        assert_eq!(snapshot.source, Some(UsageSource::ClaudeDesktop));
        assert_eq!(snapshot.windows[0].resets_at, Some(resets[0]));
        assert_eq!(snapshot.windows[1].resets_at, Some(resets[1]));

        // A cache update must be noticed even if the history file is unchanged.
        cache::write_test_report(&directory.0, ORG, NOW + 1, [3.0, 4.0], resets);
        let snapshot = reader.snapshot(NOW + 1).unwrap();
        assert_eq!(snapshot.updated_at, Some(NOW + 1));
        assert_eq!(snapshot.windows[0].remaining_percent, 97.0);
        assert_eq!(snapshot.windows[1].resets_at, Some(resets[1]));

        // An older HTTP report cannot lend its resets to newer percentages.
        directory.history(json!([sample(NOW + 10, ORG, 7.0, 8.0)]));
        let snapshot = reader.snapshot(NOW + 10).unwrap();
        assert_eq!(snapshot.windows[0].remaining_percent, 93.0);
        assert!(
            snapshot
                .windows
                .iter()
                .all(|window| window.resets_at.is_none())
        );

        // Date resolution/network delay is allowed only for matching readings.
        cache::write_test_report(&directory.0, ORG, NOW + 9, [7.0, 8.0], resets);
        let snapshot = reader.snapshot(NOW + 10).unwrap();
        assert_eq!(snapshot.updated_at, Some(NOW + 9));
        assert_eq!(snapshot.windows[0].resets_at, Some(resets[0]));
        cache::write_test_report(&directory.0, ORG, NOW + 9, [7.0, 9.0], resets);
        let snapshot = reader.snapshot(NOW + 10).unwrap();
        assert!(
            snapshot
                .windows
                .iter()
                .all(|window| window.resets_at.is_none())
        );

        // Matching percentages alone cannot bridge a reset between reports.
        cache::write_test_report(&directory.0, ORG, NOW + 8, [7.0, 8.0], [NOW + 9, resets[1]]);
        let snapshot = reader.snapshot(NOW + 10).unwrap();
        assert!(
            snapshot
                .windows
                .iter()
                .all(|window| window.resets_at.is_none())
        );
    }

    #[test]
    fn cached_usage_respects_logout_and_organization_changes() {
        const ORG_A: &str = "11111111-1111-4111-8111-111111111111";
        const ORG_B: &str = "22222222-2222-4222-8222-222222222222";
        let directory = Directory::new();
        directory.config("account-a", true);
        directory.history(json!([sample(NOW, ORG_A, 1.0, 2.0)]));
        let resets = [NOW + 3600, NOW + 86400];
        cache::write_test_report(&directory.0, ORG_A, NOW, [1.0, 2.0], resets);
        let mut reader = directory.reader();
        assert!(reader.snapshot(NOW).unwrap().windows[0].resets_at.is_some());
        directory.config("account-a", false);
        assert!(reader.snapshot(NOW + 1).is_none());
        directory.config("account-b", true);
        assert!(reader.snapshot(NOW + 2).is_none());
        directory.history(json!([sample(NOW + 3, ORG_B, 3.0, 4.0)]));
        let snapshot = reader.snapshot(NOW + 3).unwrap();
        assert_eq!(snapshot.windows[0].remaining_percent, 97.0);
        assert!(
            snapshot
                .windows
                .iter()
                .all(|window| window.resets_at.is_none())
        );
        cache::write_test_report(&directory.0, ORG_B, NOW + 3, [3.0, 4.0], resets);
        let snapshot = reader.snapshot(NOW + 3).unwrap();
        assert_eq!(snapshot.windows[0].resets_at, Some(resets[0]));
        assert_eq!(snapshot.windows[0].remaining_percent, 97.0);
    }

    #[test]
    fn malformed_versions_identities_percentages_and_bounds_are_rejected() {
        let document = |version, samples| {
            serde_json::to_vec(&json!({"version":version,"samples":samples})).unwrap()
        };
        assert!(parse_report(&document(1, json!([sample(NOW, "org-a", 1.0, 2.0)]))).is_none());
        for organization in ["", " "] {
            assert!(
                parse_report(&document(2, json!([sample(NOW, organization, 1.0, 2.0)]))).is_none()
            );
        }
        for percent in [-1.0, 100.1] {
            assert!(
                parse_report(&document(2, json!([sample(NOW, "org-a", percent, 2.0)]))).is_none()
            );
            assert!(
                parse_report(&document(2, json!([sample(NOW, "org-a", 1.0, percent)]))).is_none()
            );
        }
        assert!(
            parse_report(br#"{"version":2,"samples":[{"t":1,"org":"a","u":{"fh":1e999,"sd":0}}]}"#)
                .is_none()
        );
        assert!(parse_report(&vec![b' '; MAX_HISTORY_BYTES as usize + 1]).is_none());
        assert!(
            parse_report(&document(
                2,
                json!(vec![sample(NOW, "org-a", 1.0, 2.0); MAX_SAMPLES + 1])
            ))
            .is_none()
        );
        let directory = Directory::new();
        directory.config("account-a", true);
        directory.history(json!([sample(NOW, "org-a", 1.0, 2.0)]));
        let mut reader = directory.reader();
        assert!(reader.snapshot(NOW).is_some());
        File::create(directory.0.join("config.json"))
            .unwrap()
            .set_len(MAX_CONFIG_BYTES + 1)
            .unwrap();
        assert!(reader.snapshot(NOW).is_none());
        assert!(reader.report.is_none());
        directory.config("account-a", true);
        File::create(directory.0.join("plan-usage-history.json"))
            .unwrap()
            .set_len(MAX_HISTORY_BYTES + 1)
            .unwrap();
        assert!(directory.reader().snapshot(NOW).is_none());
    }

    #[test]
    #[ignore = "Reads the current user's Claude Desktop usage report"]
    fn installed_desktop_report_is_read_without_credentials() {
        let now = chrono::Utc::now().timestamp();
        let snapshot = DesktopUsageReader::default()
            .snapshot(now)
            .expect("recent signed-in Claude Desktop report");
        println!(
            "Claude Desktop report: age={}s, remaining={:?}, resets={:?}",
            now - snapshot.updated_at.unwrap(),
            snapshot
                .windows
                .iter()
                .map(|window| window.remaining_percent)
                .collect::<Vec<_>>(),
            snapshot
                .windows
                .iter()
                .map(|window| window.resets_at)
                .collect::<Vec<_>>()
        );
    }
}
