// SPDX-License-Identifier: MPL-2.0

//! Shared presentation data for account quota reports from AI applications.

pub mod claude;
pub mod codex;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UsageStatus {
    Loading,
    Available,
    Stale,
    SignInRequired,
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    /// Separate quota buckets must retain their identity.
    pub limit_id: String,
    pub label: String,
    pub remaining_percent: f32,
    /// Unix seconds supplied by the provider.
    pub resets_at: Option<i64>,
    /// Time of the report that supplied this window, in Unix seconds.
    pub updated_at: i64,
    pub is_stale: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageSnapshot {
    pub windows: Vec<UsageWindow>,
    /// Latest report represented here; individual windows retain their own age.
    pub updated_at: Option<i64>,
    pub status: UsageStatus,
}
