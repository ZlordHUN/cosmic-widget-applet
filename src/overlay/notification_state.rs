// SPDX-License-Identifier: MPL-2.0

//! Notification identities, dismissal state, and activation actions.

use super::animation::ExpansionAnimation;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(super) type NotificationKey = crate::monitors::notifications::NotificationIdentity;

#[derive(Debug, Clone)]
pub(super) struct DismissingNotification {
    pub(super) key: NotificationKey,
    pub(super) source: String,
    pub(super) animation: ExpansionAnimation,
}

impl DismissingNotification {
    pub(super) fn new(
        notification: &crate::monitors::notifications::Notification,
        now: Instant,
    ) -> Self {
        let mut animation = ExpansionAnimation::default();
        animation.transition_to(1.0, now);
        Self {
            key: notification.identity(),
            source: notification_source(notification).to_string(),
            animation,
        }
    }

    pub(super) fn matches(
        &self,
        notification: &crate::monitors::notifications::Notification,
    ) -> bool {
        self.key.matches(notification)
    }
}

pub(super) fn notification_source(
    notification: &crate::monitors::notifications::Notification,
) -> &str {
    if notification.app_name.trim().is_empty()
        || notification.app_name.eq_ignore_ascii_case("system")
    {
        notification.summary.trim()
    } else {
        notification.app_name.trim()
    }
}

pub(super) fn transition_clear_button_for_notification_change(
    animation: &mut ExpansionAnimation,
    had_notifications: bool,
    has_notifications: bool,
    clearing_notifications: bool,
    now: Instant,
) {
    if clearing_notifications || had_notifications == has_notifications {
        return;
    }

    animation.transition_to(if has_notifications { 1.0 } else { 0.0 }, now);
}

pub(super) async fn open_notification_folder(target: PathBuf) -> Result<(), String> {
    if target.is_file() {
        let cosmic_files = tokio::process::Command::new("cosmic-files")
            .arg(&target)
            .status()
            .await;
        if cosmic_files
            .as_ref()
            .is_ok_and(std::process::ExitStatus::success)
        {
            return Ok(());
        }

        let parent = target
            .parent()
            .filter(|path| path.is_dir())
            .ok_or_else(|| format!("{} does not have an accessible parent", target.display()))?;
        return open_directory(parent).await.map_err(|fallback_error| {
            let cosmic_error = match cosmic_files {
                Ok(status) => format!("cosmic-files exited with {status}"),
                Err(error) => format!("could not start cosmic-files: {error}"),
            };
            format!("{cosmic_error}; {fallback_error}")
        });
    }

    if !target.is_dir() {
        return Err(format!(
            "{} is not an accessible file or directory",
            target.display()
        ));
    }

    open_directory(&target).await
}

async fn open_directory(directory: &Path) -> Result<(), String> {
    let status = tokio::process::Command::new("xdg-open")
        .arg(directory)
        .status()
        .await
        .map_err(|error| format!("could not start xdg-open: {error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("xdg-open exited with {status}"))
}
