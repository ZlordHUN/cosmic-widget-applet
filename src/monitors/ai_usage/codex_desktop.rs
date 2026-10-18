// SPDX-License-Identifier: MPL-2.0

//! Read the small account-usage response already cached by Codex Desktop.
//! Only deterministic usage-cache paths and the local account-ID metadata are
//! inspected. This reader never accesses cookies or makes an authenticated request.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

use super::codex::{parse_credit_balance, window_label};
use super::http_cache::{cache_filename, parse_entry};
use super::{UsageCredits, UsageSnapshot, UsageStatus, UsageWindow};

const MAX_ENTRY_BYTES: u64 = 256 * 1024;
const MAX_AUTH_BYTES: u64 = 64 * 1024;
const FRESH_SECONDS: i64 = 15 * 60;
const MAX_AGE_SECONDS: i64 = 24 * 60 * 60;
const USAGE_KEYS: [&str; 2] = [
    "1/0/_dk_https://chatgpt.com https://chatgpt.com https://chatgpt.com/backend-api/wham/usage",
    "1/0/https://chatgpt.com/backend-api/wham/usage",
];

pub(super) struct DesktopUsageReader {
    auth_path: Option<PathBuf>,
    account: Option<String>,
    entries: Vec<CachedEntry>,
}

struct CachedEntry {
    key: &'static str,
    path: PathBuf,
    stamp: Option<FileStamp>,
    snapshot: Option<UsageSnapshot>,
}

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    length: u64,
    modified: SystemTime,
}

impl FileStamp {
    fn read(path: &Path, limit: u64) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        if !metadata.is_file() || metadata.len() > limit {
            return None;
        }
        Some(Self {
            length: metadata.len(),
            modified: metadata.modified().ok()?,
        })
    }
}

impl DesktopUsageReader {
    pub(super) fn new(codex_home: Option<PathBuf>) -> Self {
        Self::at_paths(
            codex_home.map(|home| home.join("auth.json")),
            dirs::cache_dir().map(|cache| cache.join("Codex/Default/Cache/Cache_Data")),
        )
    }

    fn at_paths(auth_path: Option<PathBuf>, cache_directory: Option<PathBuf>) -> Self {
        let entries = cache_directory
            .into_iter()
            .flat_map(|directory| {
                USAGE_KEYS.map(|key| CachedEntry {
                    key,
                    path: directory.join(cache_filename(key)),
                    stamp: None,
                    snapshot: None,
                })
            })
            .collect();
        Self {
            auth_path,
            account: None,
            entries,
        }
    }

    fn clear_entries(&mut self) {
        for entry in &mut self.entries {
            entry.stamp = None;
            entry.snapshot = None;
        }
    }

    pub(super) fn snapshot(&mut self, now: i64) -> Option<UsageSnapshot> {
        let account = self.auth_path.as_deref().and_then(read_account);
        if self.account != account {
            self.account = account;
            self.clear_entries();
        }
        let account = self.account.as_deref()?;
        for entry in &mut self.entries {
            let stamp = FileStamp::read(&entry.path, MAX_ENTRY_BYTES);
            if stamp.is_none() {
                entry.stamp = None;
                entry.snapshot = None;
                continue;
            }
            if stamp != entry.stamp {
                entry.snapshot = read_bounded(&entry.path, MAX_ENTRY_BYTES)
                    .and_then(|bytes| parse_entry(&bytes, entry.key, now, MAX_AGE_SECONDS))
                    .and_then(|response| {
                        parse_usage(&response.body, account, response.updated_at, now)
                    });
                if FileStamp::read(&entry.path, MAX_ENTRY_BYTES) == stamp {
                    // Recheck rejected responses on the next refresh. A future
                    // HTTP Date may become valid when the local clock catches up.
                    entry.stamp = if entry.snapshot.is_some() {
                        stamp
                    } else {
                        None
                    };
                } else {
                    // Desktop may replace its response while we are reading it.
                    entry.stamp = None;
                    entry.snapshot = None;
                }
            }
        }
        // Do not publish the previous account if sign-out/switching raced a read.
        if self.auth_path.as_deref().and_then(read_account).as_deref() != Some(account) {
            self.account = None;
            self.clear_entries();
            return None;
        }
        self.entries
            .iter()
            .filter_map(|entry| entry.snapshot.as_ref())
            .filter_map(|snapshot| present(snapshot.clone(), now))
            .max_by_key(|snapshot| snapshot.updated_at)
    }
}

fn read_bounded(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

#[derive(Deserialize)]
struct AccountMetadata {
    tokens: Option<AccountTokensMetadata>,
}

#[derive(Deserialize)]
struct AccountTokensMetadata {
    account_id: Option<String>,
}

fn read_account(path: &Path) -> Option<String> {
    let stamp = FileStamp::read(path, MAX_AUTH_BYTES)?;
    // Serde ignores credential fields: only the non-secret account identity is
    // retained, and neither it nor the response's personal fields are logged.
    let metadata: AccountMetadata =
        serde_json::from_slice(&read_bounded(path, MAX_AUTH_BYTES)?).ok()?;
    if FileStamp::read(path, MAX_AUTH_BYTES).as_ref() != Some(&stamp) {
        return None;
    }
    let account = metadata.tokens?.account_id?;
    (!account.is_empty() && account.len() <= 256 && account.trim() == account).then_some(account)
}

#[derive(Deserialize)]
struct DesktopUsage {
    account_id: String,
    rate_limit: Option<DesktopLimits>,
    #[serde(default)]
    credits: serde_json::Value,
}

#[derive(Deserialize)]
struct DesktopLimits {
    #[serde(default)]
    primary_window: serde_json::Value,
    #[serde(default)]
    secondary_window: serde_json::Value,
}

#[derive(Deserialize)]
struct DesktopWindow {
    used_percent: f64,
    limit_window_seconds: i64,
    reset_at: i64,
}

fn parse_usage(bytes: &[u8], account: &str, updated_at: i64, now: i64) -> Option<UsageSnapshot> {
    let usage: DesktopUsage = serde_json::from_slice(bytes).ok()?;
    if account.is_empty() || usage.account_id != account {
        return None;
    }
    let mut snapshot = UsageSnapshot {
        updated_at: Some(updated_at),
        credits: parse_credit_balance(&usage.credits).map(|balance| UsageCredits {
            balance,
            updated_at,
            is_stale: false,
        }),
        ..UsageSnapshot::default()
    };
    if let Some(limits) = usage.rate_limit {
        for (value, fallback) in [
            (limits.primary_window, "Primary"),
            (limits.secondary_window, "Secondary"),
        ] {
            let Ok(window) = serde_json::from_value::<DesktopWindow>(value) else {
                continue;
            };
            if !window.used_percent.is_finite()
                || !(0.0..=100.0).contains(&window.used_percent)
                || window.limit_window_seconds <= 0
                || window.limit_window_seconds % 60 != 0
                || window.reset_at <= 0
            {
                continue;
            }
            snapshot.windows.push(UsageWindow {
                limit_id: "codex".into(),
                label: window_label(Some(window.limit_window_seconds / 60), fallback),
                remaining_percent: (100.0 - window.used_percent) as f32,
                resets_at: Some(window.reset_at),
                updated_at,
                is_stale: false,
            });
        }
    }
    if snapshot.windows.is_empty() && snapshot.credits.is_none() {
        return None;
    }
    present(snapshot, now)
}

fn present(mut snapshot: UsageSnapshot, now: i64) -> Option<UsageSnapshot> {
    let age = now.checked_sub(snapshot.updated_at?)?;
    if !(0..=MAX_AGE_SECONDS).contains(&age) {
        return None;
    }
    for window in &mut snapshot.windows {
        window.is_stale = age > FRESH_SECONDS || window.resets_at.is_some_and(|reset| reset <= now);
    }
    if let Some(credits) = &mut snapshot.credits {
        credits.is_stale = age > FRESH_SECONDS;
    }
    snapshot.status = if snapshot.windows.iter().any(|window| window.is_stale)
        || snapshot
            .credits
            .as_ref()
            .is_some_and(|credits| credits.is_stale)
    {
        UsageStatus::Stale
    } else {
        UsageStatus::Available
    };
    Some(snapshot)
}

#[cfg(test)]
mod tests {
    use super::super::CreditBalance;
    use super::super::http_cache::test_entry;
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    const NOW: i64 = 1_800_000_000;
    const ACCOUNT: &str = "test-account-one";

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "cosmic-widget-codex-desktop-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("cache")).unwrap();
            let fixture = Self(path);
            fixture.account(ACCOUNT);
            fixture
        }

        fn account(&self, account: &str) {
            fs::write(
                self.0.join("auth.json"),
                serde_json::to_vec(&json!({"tokens": {"account_id": account}})).unwrap(),
            )
            .unwrap();
        }

        fn report(&self, timestamp: i64, body: serde_json::Value) {
            self.report_key(USAGE_KEYS[0], timestamp, body);
        }

        fn report_key(&self, key: &str, timestamp: i64, body: serde_json::Value) {
            fs::write(
                self.0.join("cache").join(cache_filename(key)),
                test_entry(
                    key,
                    timestamp,
                    "identity",
                    &serde_json::to_vec(&body).unwrap(),
                ),
            )
            .unwrap();
        }

        fn reader(&self) -> DesktopUsageReader {
            DesktopUsageReader::at_paths(Some(self.0.join("auth.json")), Some(self.0.join("cache")))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn body(used: f64, reset: i64) -> serde_json::Value {
        json!({
            "account_id": ACCOUNT,
            "rate_limit": {
                "primary_window": {
                    "used_percent": used,
                    "limit_window_seconds": 604800,
                    "reset_at": reset
                },
                "secondary_window": null
            },
            "credits": null
        })
    }

    #[test]
    fn desktop_response_updates_after_reset_without_session_log_changes() {
        let fixture = Fixture::new();
        fixture.report(NOW - 60, body(100.0, NOW - 1));
        let mut reader = fixture.reader();
        let old = reader.snapshot(NOW).unwrap();
        assert_eq!(old.windows[0].remaining_percent, 0.0);
        assert_eq!(old.status, UsageStatus::Stale);

        fixture.report(NOW, body(12.5, NOW + 604800));
        let current = reader.snapshot(NOW).unwrap();
        assert_eq!(current.windows[0].remaining_percent, 87.5);
        assert_eq!(current.windows[0].resets_at, Some(NOW + 604800));
        assert_eq!(current.windows[0].label, "Weekly");
        assert_eq!(current.status, UsageStatus::Available);
    }

    #[test]
    fn unchanged_response_ages_and_never_assumes_a_reset_refill() {
        let fixture = Fixture::new();
        fixture.report(NOW, body(80.0, NOW + 10));
        let mut reader = fixture.reader();
        assert_eq!(reader.snapshot(NOW).unwrap().status, UsageStatus::Available);
        let reset = reader.snapshot(NOW + 10).unwrap();
        assert_eq!(reset.status, UsageStatus::Stale);
        assert_eq!(reset.windows[0].remaining_percent, 20.0);
        assert!(reader.snapshot(NOW + MAX_AGE_SECONDS + 1).is_none());
    }

    #[test]
    fn unchanged_future_dated_response_is_retried_when_clock_catches_up() {
        let fixture = Fixture::new();
        fixture.report(NOW + 10, body(25.0, NOW + 604800));
        let path = fixture.0.join("cache").join(cache_filename(USAGE_KEYS[0]));
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let mut reader = fixture.reader();
        assert!(reader.snapshot(NOW).is_none());

        let snapshot = reader.snapshot(NOW + 30).unwrap();
        assert_eq!(snapshot.status, UsageStatus::Available);
        assert_eq!(snapshot.windows[0].remaining_percent, 75.0);
        assert_eq!(snapshot.updated_at, Some(NOW + 10));
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
    }

    #[test]
    fn older_response_is_stale_before_its_reset() {
        let snapshot = parse_usage(
            &serde_json::to_vec(&body(45.0, NOW + 604800)).unwrap(),
            ACCOUNT,
            NOW - FRESH_SECONDS - 1,
            NOW,
        )
        .unwrap();
        assert_eq!(snapshot.status, UsageStatus::Stale);
        assert_eq!(snapshot.windows[0].remaining_percent, 55.0);
    }

    #[test]
    fn sign_out_and_account_switch_invalidate_unchanged_cache() {
        let fixture = Fixture::new();
        fixture.report(NOW, body(20.0, NOW + 604800));
        let mut reader = fixture.reader();
        assert!(reader.snapshot(NOW).is_some());
        fs::remove_file(fixture.0.join("auth.json")).unwrap();
        assert!(reader.snapshot(NOW).is_none());
        fixture.account("test-account-two");
        assert!(reader.snapshot(NOW).is_none());
        fixture.account(ACCOUNT);
        assert!(reader.snapshot(NOW).is_some());
        fixture.account("");
        assert!(reader.snapshot(NOW).is_none());
    }

    #[test]
    fn missing_or_malformed_account_metadata_does_not_publish_usage() {
        let fixture = Fixture::new();
        fixture.report(NOW, body(20.0, NOW + 604800));
        let mut reader = fixture.reader();
        for auth in [
            json!({}),
            json!({"tokens": null}),
            json!({"tokens": {"account_id": 123}}),
        ] {
            fs::write(
                fixture.0.join("auth.json"),
                serde_json::to_vec(&auth).unwrap(),
            )
            .unwrap();
            assert!(reader.snapshot(NOW).is_none());
        }
        fs::write(
            fixture.0.join("auth.json"),
            vec![b' '; MAX_AUTH_BYTES as usize + 1],
        )
        .unwrap();
        assert!(reader.snapshot(NOW).is_none());
    }

    #[test]
    fn malformed_quota_values_are_not_displayed() {
        for (key, value) in [
            ("used_percent", json!(-1)),
            ("used_percent", json!(101)),
            ("used_percent", json!("50")),
            ("limit_window_seconds", json!(0)),
            ("limit_window_seconds", json!(-60)),
            ("limit_window_seconds", json!(61)),
            ("limit_window_seconds", json!(3600.5)),
            ("reset_at", json!(0)),
            ("reset_at", json!(-1)),
            ("reset_at", json!(null)),
        ] {
            let mut report = body(20.0, NOW + 604800);
            report["rate_limit"]["primary_window"][key] = value;
            assert!(
                parse_usage(&serde_json::to_vec(&report).unwrap(), ACCOUNT, NOW, NOW).is_none(),
                "{key}"
            );
        }
        let mut report = body(20.0, NOW + 604800);
        report["rate_limit"]["primary_window"]
            .as_object_mut()
            .unwrap()
            .remove("reset_at");
        assert!(parse_usage(&serde_json::to_vec(&report).unwrap(), ACCOUNT, NOW, NOW).is_none());
    }

    #[test]
    fn valid_secondary_window_survives_malformed_primary() {
        let mut report = body(20.0, NOW + 604800);
        report["rate_limit"]["secondary_window"] = json!({
            "used_percent": 33,
            "limit_window_seconds": 18000,
            "reset_at": NOW + 18000,
        });
        report["rate_limit"]["primary_window"]["used_percent"] = json!(101);
        let snapshot =
            parse_usage(&serde_json::to_vec(&report).unwrap(), ACCOUNT, NOW, NOW).unwrap();
        assert_eq!(snapshot.windows.len(), 1);
        assert_eq!(snapshot.windows[0].remaining_percent, 67.0);
        assert_eq!(snapshot.windows[0].label, "5 hours");
    }

    #[test]
    fn missing_or_unavailable_credits_are_not_zero() {
        let mut report = body(20.0, NOW + 604800);
        for credits in [
            json!(null),
            json!({}),
            json!({"has_credits": true, "unlimited": false, "balance": null}),
        ] {
            report["credits"] = credits;
            let snapshot =
                parse_usage(&serde_json::to_vec(&report).unwrap(), ACCOUNT, NOW, NOW).unwrap();
            assert!(snapshot.credits.is_none());
        }
        report.as_object_mut().unwrap().remove("credits");
        assert!(
            parse_usage(&serde_json::to_vec(&report).unwrap(), ACCOUNT, NOW, NOW)
                .unwrap()
                .credits
                .is_none()
        );
        report["credits"] = json!({"has_credits": false, "unlimited": false, "balance": "0.0000"});
        let snapshot =
            parse_usage(&serde_json::to_vec(&report).unwrap(), ACCOUNT, NOW, NOW).unwrap();
        assert_eq!(
            snapshot.credits.unwrap().balance,
            CreditBalance::Amount("0.0000".into())
        );
    }

    #[test]
    fn latest_compatible_cache_key_wins_without_including_other_buckets() {
        let fixture = Fixture::new();
        fixture.report(NOW - 60, body(40.0, NOW + 604800));
        let mut report = body(55.0, NOW + 604800);
        report["additional_rate_limits"] = json!([{"limit_name": "other", "rate_limit": {"primary_window": {"used_percent": 99, "limit_window_seconds": 604800, "reset_at": NOW + 604800}}}]);
        fixture.report_key(USAGE_KEYS[1], NOW, report);
        let snapshot = fixture.reader().snapshot(NOW).unwrap();
        assert_eq!(snapshot.windows.len(), 1);
        assert_eq!(snapshot.windows[0].limit_id, "codex");
        assert_eq!(snapshot.windows[0].remaining_percent, 45.0);
    }

    #[test]
    #[ignore = "reads this user's local Codex Desktop usage cache"]
    fn reads_live_desktop_usage_without_network_access() {
        let codex_home = std::env::var_os("CODEX_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".codex")));
        let snapshot = DesktopUsageReader::new(codex_home)
            .snapshot(chrono::Utc::now().timestamp())
            .expect("a matching recent Desktop usage response");
        for window in snapshot.windows {
            println!(
                "{}: {}% left, reset {:?}, updated {}",
                window.label, window.remaining_percent, window.resets_at, window.updated_at
            );
        }
        println!("credit balance available: {}", snapshot.credits.is_some());
    }
}
