// SPDX-License-Identifier: MPL-2.0

//! COSMIC Files copy/move notification metadata. Other applications and Files
//! operations deliberately remain ordinary desktop notifications.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zbus::zvariant::OwnedValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileTransferState {
    Running,
    Paused,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTransfer {
    pub progress: u8,
    pub state: FileTransferState,
}

impl FileTransfer {
    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            FileTransferState::Running | FileTransferState::Paused
        )
    }
}

pub(super) fn from_hints(
    app_name: &str,
    hints: &HashMap<String, OwnedValue>,
) -> Option<FileTransfer> {
    let desktop_entry = super::notification_string_hint(hints, "desktop-entry")?;
    if app_name != "COSMIC Files"
        || desktop_entry.trim_end_matches(".desktop") != "com.system76.CosmicFiles"
    {
        return None;
    }
    let operation = super::notification_string_hint(hints, "x-cosmic-files-operation")?;
    if !matches!(operation.as_str(), "copy" | "move") {
        return None;
    }
    let state = match super::notification_string_hint(hints, "x-cosmic-files-state")?.as_str() {
        "running" => FileTransferState::Running,
        "paused" => FileTransferState::Paused,
        "completed" => FileTransferState::Completed,
        "cancelled" => FileTransferState::Cancelled,
        "failed" => FileTransferState::Failed,
        _ => return None,
    };
    let progress = hints.get("value")?.try_clone().ok()?;
    let progress = i32::try_from(progress).ok()?;
    let progress = u8::try_from(progress)
        .ok()
        .filter(|progress| *progress <= 100)?;
    Some(FileTransfer { progress, state })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use zbus::zvariant::Value;

    pub(crate) fn hints(
        operation: &str,
        state: &str,
        progress: i32,
    ) -> HashMap<String, OwnedValue> {
        let mut hints = HashMap::new();
        for (name, value) in [
            ("desktop-entry", "com.system76.CosmicFiles"),
            ("x-cosmic-files-operation", operation),
            ("x-cosmic-files-state", state),
        ] {
            hints.insert(
                name.to_string(),
                OwnedValue::try_from(Value::from(value)).unwrap(),
            );
        }
        hints.insert("value".to_string(), progress.into());
        hints
    }

    #[test]
    fn accepts_only_explicit_cosmic_files_copy_and_move_metadata() {
        for operation in ["copy", "move"] {
            let transfer = from_hints("COSMIC Files", &hints(operation, "running", 42)).unwrap();
            assert_eq!(transfer.progress, 42);
            assert!(transfer.is_active());
        }
        assert!(from_hints("Another file manager", &hints("copy", "running", 42)).is_none());
        assert!(from_hints("COSMIC Files", &hints("delete", "running", 42)).is_none());
        let mut missing_identity = hints("copy", "running", 42);
        missing_identity.remove("desktop-entry");
        assert!(from_hints("COSMIC Files", &missing_identity).is_none());
        missing_identity.insert(
            "desktop-entry".to_string(),
            OwnedValue::try_from(Value::from("org.gnome.Nautilus")).unwrap(),
        );
        assert!(from_hints("COSMIC Files", &missing_identity).is_none());
    }

    #[test]
    fn validates_progress_and_preserves_terminal_outcomes() {
        for progress in [-1, 101, i32::MAX] {
            assert!(from_hints("COSMIC Files", &hints("copy", "running", progress)).is_none());
        }
        for (state, expected) in [
            ("paused", FileTransferState::Paused),
            ("completed", FileTransferState::Completed),
            ("cancelled", FileTransferState::Cancelled),
            ("failed", FileTransferState::Failed),
        ] {
            assert_eq!(
                from_hints("COSMIC Files", &hints("move", state, 70))
                    .unwrap()
                    .state,
                expected
            );
        }
        assert!(from_hints("COSMIC Files", &hints("copy", "unknown", 50)).is_none());
        let mut invalid_type = hints("copy", "running", 42);
        invalid_type.insert(
            "value".to_string(),
            OwnedValue::try_from(Value::from("42")).unwrap(),
        );
        assert!(from_hints("COSMIC Files", &invalid_type).is_none());
    }
}
