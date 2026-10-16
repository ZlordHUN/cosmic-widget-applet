// SPDX-License-Identifier: MPL-2.0

//! Shared presentation data for account quota reports from AI applications.

pub mod claude;
mod claude_cache;
mod claude_desktop;
pub mod codex;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UsageStatus {
    Loading,
    Available,
    Stale,
    SignInRequired,
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// A purchased credit balance is an amount, never a percentage of a quota.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CreditBalance {
    /// Keep the provider's decimal string without floating-point rounding.
    Amount(String),
    Unlimited,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsageCredits {
    pub balance: CreditBalance,
    /// Time of the report that supplied the balance, in Unix seconds.
    pub updated_at: i64,
    pub is_stale: bool,
}

/// Claude report sources have different metadata and update schedules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UsageSource {
    ClaudeCode,
    ClaudeDesktop,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UsageSnapshot {
    pub windows: Vec<UsageWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<UsageCredits>,
    /// Latest quota report represented here; windows and credits retain their own age.
    pub updated_at: Option<i64>,
    pub status: UsageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<UsageSource>,
}
