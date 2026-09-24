// SPDX-License-Identifier: MPL-2.0

//! Read Claude's account quota using the same read-only endpoint as Claude Code.
//!
//! Only the existing Claude Code access token is read. This monitor never
//! refreshes credentials, starts an inference request, or writes a login file.
//! Requests are restricted to Anthropic's fixed HTTPS endpoint, with redirects
//! disabled. Tokens and response bodies must never be logged.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, HeaderValue, RETRY_AFTER};
use serde::Deserialize;

use super::{UsageSnapshot, UsageStatus, UsageWindow};

const USAGE_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const POLL_INTERVAL: Duration = Duration::from_secs(30);
const REQUEST_INTERVAL_SECS: i64 = 5 * 60;
const MAX_BACKOFF_SECS: i64 = 60 * 60;
const STALE_AFTER_SECS: i64 = 15 * 60;
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;

#[derive(Clone)]
pub struct ClaudeUsageMonitor {
    snapshot: Arc<Mutex<UsageSnapshot>>,
    enabled: Arc<AtomicBool>,
    wake: Sender<()>,
}

impl ClaudeUsageMonitor {
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
        let credentials = credential_path();
        let worker = std::thread::Builder::new()
            .name("claude-usage".into())
            .spawn(move || {
                let client = usage_client().ok();
                let mut reader = UsageReader::default();
                loop {
                    if worker_enabled.load(Ordering::Relaxed) {
                        let value = match (credentials.as_ref(), client.as_ref()) {
                            (Some(path), Some(client)) => reader.refresh(
                                path,
                                chrono::Utc::now().timestamp(),
                                |token, now| fetch_usage(client, token, now),
                            ),
                            (None, _) => sign_in_required(),
                            _ => UsageSnapshot::default(),
                        };
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
            log::warn!("Unable to start Claude usage monitor");
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

fn credential_path() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .map(|home| home.join(".credentials.json"))
}

fn usage_client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("cosmic-widget/", env!("CARGO_PKG_VERSION")))
        .build()
}

// Deliberately no Debug: access tokens must never enter logs or error messages.
#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: OAuthCredentials,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthCredentials {
    access_token: String,
    expires_at: i64,
    #[serde(default)]
    scopes: Vec<String>,
}

#[derive(Clone, PartialEq, Eq)]
struct CredentialStamp {
    modified: SystemTime,
    length: u64,
}

fn credential_stamp(path: &Path) -> Option<CredentialStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(CredentialStamp {
        modified: metadata.modified().ok()?,
        length: metadata.len(),
    })
}

fn read_credentials(path: &Path, now: i64) -> Option<(OAuthCredentials, CredentialStamp)> {
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    let stamp = CredentialStamp {
        modified: metadata.modified().ok()?,
        length: metadata.len(),
    };
    let bytes = read_bounded(file, MAX_CREDENTIAL_BYTES)?;
    let credentials: Credentials = serde_json::from_slice(&bytes).ok()?;
    let oauth = credentials.oauth;
    // Only profile access is required. Claude Code owns refreshing this token.
    if oauth.access_token.is_empty()
        || oauth.expires_at / 1000 <= now
        || !oauth.scopes.iter().any(|scope| scope == "user:profile")
    {
        return None;
    }
    Some((oauth, stamp))
}

fn read_bounded(reader: impl Read, max_bytes: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(max_bytes + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= max_bytes).then_some(bytes)
}

#[derive(Debug)]
enum FetchError {
    SignInRequired,
    Retry { after_secs: Option<i64> },
}

fn fetch_usage(client: &Client, token: &str, now: i64) -> Result<UsageSnapshot, FetchError> {
    let retry = || FetchError::Retry { after_secs: None };
    let mut authorization =
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| retry())?;
    authorization.set_sensitive(true);
    let response = client
        .get(USAGE_ENDPOINT)
        .header(AUTHORIZATION, authorization)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("accept", "application/json")
        .send()
        .map_err(|_| retry())?;
    match response.status().as_u16() {
        401 | 403 => return Err(FetchError::SignInRequired),
        200 => {}
        _ => {
            return Err(FetchError::Retry {
                after_secs: response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| retry_after_secs(value, now)),
            });
        }
    }
    let bytes = read_bounded(response, MAX_RESPONSE_BYTES).ok_or_else(retry)?;
    parse_usage(&bytes, now).ok_or_else(retry)
}

fn retry_after_secs(value: &str, now: i64) -> Option<i64> {
    value.trim().parse::<i64>().ok().or_else(|| {
        chrono::DateTime::parse_from_rfc2822(value)
            .ok()
            .map(|time| time.timestamp().saturating_sub(now))
    })
}

fn parse_usage(bytes: &[u8], now: i64) -> Option<UsageSnapshot> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let value = value.as_object()?;
    let mut windows = Vec::new();
    for (id, label) in [
        ("five_hour", "5-hour"),
        ("seven_day", "Weekly"),
        ("seven_day_sonnet", "Sonnet · Weekly"),
        ("seven_day_opus", "Opus · Weekly"),
        ("seven_day_cowork", "Cowork · Weekly"),
        ("seven_day_oauth_apps", "OAuth apps · Weekly"),
    ] {
        let Some(window) = value.get(id).and_then(|value| value.as_object()) else {
            continue;
        };
        let Some(used) = window.get("utilization").and_then(|value| value.as_f64()) else {
            continue;
        };
        if !used.is_finite() || !(0.0..=100.0).contains(&used) {
            continue;
        }
        let resets_at = match window.get("resets_at") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(value)) => {
                let Ok(time) = chrono::DateTime::parse_from_rfc3339(value) else {
                    continue;
                };
                if time.timestamp() <= 0 {
                    continue;
                }
                Some(time.timestamp())
            }
            _ => continue,
        };
        windows.push(UsageWindow {
            limit_id: id.into(),
            label: label.into(),
            remaining_percent: (100.0 - used) as f32,
            resets_at,
            updated_at: now,
            is_stale: resets_at.is_some_and(|reset| reset <= now),
        });
    }
    if windows.is_empty() {
        return None;
    }
    let status = if windows.iter().any(|window| window.is_stale) {
        UsageStatus::Stale
    } else {
        UsageStatus::Available
    };
    Some(UsageSnapshot {
        windows,
        updated_at: Some(now),
        status,
    })
}

#[derive(Default)]
struct UsageReader {
    stamp: Option<CredentialStamp>,
    snapshot: UsageSnapshot,
    next_request_at: i64,
    failures: u32,
}

impl UsageReader {
    fn refresh(
        &mut self,
        credentials_path: &Path,
        now: i64,
        fetch: impl FnOnce(&str, i64) -> Result<UsageSnapshot, FetchError>,
    ) -> UsageSnapshot {
        let Some((credentials, stamp)) = read_credentials(credentials_path, now) else {
            // A logged-out or expired session must not show another account's
            // last successful quota, even when the desktop cache still exists.
            *self = Self::default();
            self.snapshot = sign_in_required();
            return self.snapshot.clone();
        };
        if self.stamp.as_ref() != Some(&stamp) {
            *self = Self::default();
            self.stamp = Some(stamp);
        }
        if now >= self.next_request_at {
            let result = fetch(&credentials.access_token, now);
            // A login can change while the network request is in flight. Never
            // publish that response as the newly selected account's quota.
            let current_stamp = credential_stamp(credentials_path);
            if current_stamp != self.stamp {
                *self = Self::default();
                self.snapshot.status = if current_stamp.is_some() {
                    UsageStatus::Loading
                } else {
                    UsageStatus::SignInRequired
                };
                return self.snapshot.clone();
            }
            match result {
                Ok(snapshot) => {
                    self.snapshot = snapshot;
                    self.failures = 0;
                    self.next_request_at = now.saturating_add(REQUEST_INTERVAL_SECS);
                }
                Err(FetchError::SignInRequired) => {
                    self.snapshot = sign_in_required();
                    // Retry when Claude Code updates its credentials, without
                    // repeatedly sending the same rejected access token.
                    self.next_request_at = i64::MAX;
                }
                Err(FetchError::Retry { after_secs }) => {
                    self.failures = self.failures.saturating_add(1);
                    let delay = retry_delay(self.failures, after_secs);
                    self.next_request_at = now.saturating_add(delay);
                    self.mark_stale();
                }
            }
        }
        for window in &mut self.snapshot.windows {
            window.is_stale |= now.saturating_sub(window.updated_at) > STALE_AFTER_SECS
                || window.resets_at.is_some_and(|reset| reset <= now);
        }
        if self.snapshot.windows.iter().any(|window| window.is_stale) {
            self.snapshot.status = UsageStatus::Stale;
        }
        self.snapshot.clone()
    }

    fn mark_stale(&mut self) {
        for window in &mut self.snapshot.windows {
            window.is_stale = true;
        }
        self.snapshot.status = if self.snapshot.windows.is_empty() {
            UsageStatus::Unavailable
        } else {
            UsageStatus::Stale
        };
    }
}

fn retry_delay(failures: u32, after_secs: Option<i64>) -> i64 {
    let backoff = REQUEST_INTERVAL_SECS * (1_i64 << failures.saturating_sub(1).min(4));
    backoff
        .clamp(REQUEST_INTERVAL_SECS, MAX_BACKOFF_SECS)
        // The cap applies to our own retry policy, never to a server's request
        // to wait longer. Scheduling uses saturating_add for extreme values.
        .max(after_secs.unwrap_or_default())
}

fn sign_in_required() -> UsageSnapshot {
    UsageSnapshot {
        status: UsageStatus::SignInRequired,
        ..UsageSnapshot::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_280_000;

    fn report() -> UsageSnapshot {
        parse_usage(
            br#"{"five_hour":{"utilization":8,"resets_at":"2026-09-25T03:10:00.264624+00:00"},"seven_day":{"utilization":1,"resets_at":"2026-09-29T18:00:00.264647+00:00"}}"#,
            NOW,
        )
        .unwrap()
    }

    struct CredentialFile(PathBuf);

    impl CredentialFile {
        fn new() -> Self {
            static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let file = Self(std::env::temp_dir().join(format!(
                "cosmic-claude-usage-test-{}-{id}.json",
                std::process::id()
            )));
            file.write("test-access-token", NOW + 3600);
            file
        }

        fn write(&self, token: &str, expires_at: i64) {
            std::fs::write(
                &self.0,
                serde_json::to_vec(&serde_json::json!({
                    "claudeAiOauth": {
                        "accessToken": token,
                        "expiresAt": expires_at * 1000,
                        "scopes": ["user:profile"]
                    }
                }))
                .unwrap(),
            )
            .unwrap();
        }
    }

    impl Drop for CredentialFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn parses_actual_quota_and_reset_times() {
        let snapshot = report();
        assert_eq!(snapshot.status, UsageStatus::Available);
        assert_eq!(snapshot.windows.len(), 2);
        assert_eq!(snapshot.windows[0].remaining_percent, 92.0);
        assert_eq!(snapshot.windows[1].remaining_percent, 99.0);
        assert_eq!(snapshot.windows[0].resets_at, Some(1_790_305_800));
        assert_eq!(snapshot.updated_at, Some(NOW));
    }

    #[test]
    fn ignores_unknown_spend_fields_and_invalid_windows() {
        let snapshot = parse_usage(
            br#"{"five_hour":{"utilization":101},"seven_day":{"utilization":-1},"seven_day_opus":{"utilization":20,"resets_at":"invalid"},"seven_day_sonnet":{"utilization":12.5,"resets_at":null},"extra_usage":{"utilization":99}}"#,
            NOW,
        )
        .unwrap();
        assert_eq!(snapshot.windows.len(), 1);
        assert_eq!(snapshot.windows[0].label, "Sonnet · Weekly");
        assert_eq!(snapshot.windows[0].remaining_percent, 87.5);
        assert_eq!(snapshot.windows[0].resets_at, None);
        assert!(parse_usage(br#"{"five_hour":null,"seven_day":null}"#, NOW).is_none());
        assert!(parse_usage(br#"{"five_hour":{"utilization":"8"}}"#, NOW).is_none());
    }

    #[test]
    fn never_assumes_expired_window_is_replenished() {
        let snapshot = parse_usage(
            br#"{"five_hour":{"utilization":100,"resets_at":"2026-09-24T00:00:00Z"}}"#,
            NOW,
        )
        .unwrap();
        assert_eq!(snapshot.status, UsageStatus::Stale);
        assert_eq!(snapshot.windows[0].remaining_percent, 0.0);
    }

    #[test]
    fn cached_quota_polling_and_failures_do_not_fabricate_fresh_values() {
        let file = CredentialFile::new();
        let mut reader = UsageReader::default();
        reader.refresh(&file.0, NOW, |_, _| Ok(report()));
        let cached = reader.refresh(&file.0, NOW + 30, |_, _| panic!("too soon to poll"));
        assert_eq!(cached.updated_at, Some(NOW));
        let stale = reader.refresh(&file.0, NOW + 300, |_, _| {
            Err(FetchError::Retry {
                after_secs: Some(900),
            })
        });
        assert_eq!(stale.status, UsageStatus::Stale);
        assert_eq!(stale.windows[0].remaining_percent, 92.0);
        assert_eq!(stale.updated_at, Some(NOW));
        reader.refresh(&file.0, NOW + 1100, |_, _| panic!("Retry-After ignored"));
    }

    #[test]
    fn expired_credentials_clear_data_without_sending_request() {
        let file = CredentialFile::new();
        let mut reader = UsageReader::default();
        reader.refresh(&file.0, NOW, |_, _| Ok(report()));
        let snapshot = reader.refresh(&file.0, NOW + 3600, |_, _| {
            panic!("expired credentials must not be sent")
        });
        assert_eq!(snapshot.status, UsageStatus::SignInRequired);
        assert!(snapshot.windows.is_empty());
        assert!(snapshot.updated_at.is_none());
    }

    #[test]
    fn unauthorized_session_waits_for_new_credentials_and_clears_old_account() {
        let file = CredentialFile::new();
        let mut reader = UsageReader::default();
        reader.refresh(&file.0, NOW, |_, _| Ok(report()));
        let snapshot = reader.refresh(&file.0, NOW + 300, |_, _| Err(FetchError::SignInRequired));
        assert_eq!(snapshot.status, UsageStatus::SignInRequired);
        assert!(snapshot.windows.is_empty());
        reader.refresh(&file.0, NOW + 600, |_, _| panic!("same rejected token"));
        file.write("different-account-access-token", NOW + 3600);
        let snapshot = reader.refresh(&file.0, NOW + 630, |token, _| {
            assert_eq!(token, "different-account-access-token");
            Err(FetchError::Retry { after_secs: None })
        });
        assert_eq!(snapshot.status, UsageStatus::Unavailable);
        assert!(snapshot.windows.is_empty());
    }

    #[test]
    fn account_change_during_request_discards_in_flight_response() {
        let file = CredentialFile::new();
        let mut reader = UsageReader::default();
        let snapshot = reader.refresh(&file.0, NOW, |_, _| {
            file.write("another-account-with-a-different-token", NOW + 3600);
            Ok(report())
        });
        assert_eq!(snapshot.status, UsageStatus::Loading);
        assert!(snapshot.windows.is_empty());
        let snapshot = reader.refresh(&file.0, NOW + 30, |token, _| {
            assert_eq!(token, "another-account-with-a-different-token");
            Err(FetchError::SignInRequired)
        });
        assert_eq!(snapshot.status, UsageStatus::SignInRequired);
    }

    #[test]
    fn retry_backoff_and_input_sizes_are_bounded() {
        assert_eq!(retry_delay(1, None), 300);
        assert_eq!(retry_delay(2, None), 600);
        assert_eq!(retry_delay(100, None), 3600);
        assert_eq!(retry_delay(1, Some(7200)), 7200);
        assert_eq!(retry_delay(100, Some(i64::MAX)), i64::MAX);
        assert_eq!(retry_delay(1, Some(-3)), 300);
        assert_eq!(retry_after_secs("600", NOW), Some(600));
        assert_eq!(retry_after_secs("not a date", NOW), None);
        assert_eq!(read_bounded(&b"abc"[..], 3), Some(b"abc".to_vec()));
        assert!(read_bounded(&b"abcd"[..], 3).is_none());
    }

    /// Explicit, uncharged integration check. Never included in normal tests;
    /// prints only the parsed quota and uses the production HTTPS safeguards.
    #[test]
    #[ignore = "requires local Claude login and a read-only network quota request"]
    fn live_claude_quota() {
        let path = credential_path().expect("Claude credential path unavailable");
        let now = chrono::Utc::now().timestamp();
        let (credentials, stamp) =
            read_credentials(&path, now).expect("Claude Code sign-in required");
        let client = usage_client().unwrap_or_else(|_| panic!("Unable to build quota client"));
        let snapshot = fetch_usage(&client, &credentials.access_token, now)
            .unwrap_or_else(|error| panic!("Claude quota request failed: {error:?}"));
        assert!(
            credential_stamp(&path) == Some(stamp),
            "Login changed during request"
        );
        println!("Claude quota: {snapshot:?}");
        assert!(!snapshot.windows.is_empty());
    }
}
