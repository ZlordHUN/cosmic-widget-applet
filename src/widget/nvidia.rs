// SPDX-License-Identifier: MPL-2.0

//! Process-wide NVIDIA Management Library access.

use nvml_wrapper::{Nvml, enum_wrappers::device::TemperatureSensor};
use std::path::Path;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

const NVML_RETRY_INTERVAL: Duration = Duration::from_secs(30);
const DRM_ROOT: &str = "/sys/class/drm";

#[derive(Default)]
struct NvmlState {
    instance: Option<Nvml>,
    last_attempt: Option<Instant>,
}

static NVML: LazyLock<Mutex<NvmlState>> = LazyLock::new(|| Mutex::new(NvmlState::default()));
static NVIDIA_HARDWARE_PRESENT: OnceLock<bool> = OnceLock::new();

pub(super) fn hardware_present() -> bool {
    *NVIDIA_HARDWARE_PRESENT.get_or_init(|| has_nvidia_drm_device(Path::new(DRM_ROOT)))
}

fn has_nvidia_drm_device(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };

    entries.filter_map(Result::ok).any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("card") || name.contains('-') {
            return false;
        }

        std::fs::read_to_string(entry.path().join("device/vendor"))
            .is_ok_and(|vendor| vendor.trim().eq_ignore_ascii_case("0x10de"))
    })
}

fn with_nvml<T>(query: impl FnOnce(&Nvml) -> Option<T>) -> Option<T> {
    let mut state = NVML.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    if state.instance.is_none() {
        let now = Instant::now();
        if state
            .last_attempt
            .is_some_and(|attempt| now.duration_since(attempt) < NVML_RETRY_INTERVAL)
        {
            return None;
        }
        state.last_attempt = Some(now);

        match Nvml::init() {
            Ok(nvml) => {
                log::info!("Using NVML for native NVIDIA metrics");
                state.instance = Some(nvml);
            }
            Err(error) => {
                log::warn!("NVML is unavailable; retrying later: {error}");
                return None;
            }
        }
    }

    query(state.instance.as_ref()?)
}

pub(super) fn utilization() -> Option<f32> {
    with_nvml(|nvml| {
        let count = nvml.device_count().ok()?;

        (0..count)
            .filter_map(|index| nvml.device_by_index(index).ok())
            .filter_map(|device| device.utilization_rates().ok())
            .map(|rates| rates.gpu as f32)
            .max_by(f32::total_cmp)
    })
}

pub(super) fn temperature() -> Option<f32> {
    with_nvml(|nvml| {
        let count = nvml.device_count().ok()?;

        (0..count)
            .filter_map(|index| nvml.device_by_index(index).ok())
            .filter_map(|device| device.temperature(TemperatureSensor::Gpu).ok())
            .map(|temperature| temperature as f32)
            .max_by(f32::total_cmp)
    })
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires a working NVIDIA driver and GPU"]
    fn reads_native_nvidia_metrics() {
        assert!(super::hardware_present());
        assert!(super::utilization().is_some_and(|value| (0.0..=100.0).contains(&value)));
        assert!(super::temperature().is_some_and(|value| value > 0.0));
    }
}
