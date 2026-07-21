// SPDX-License-Identifier: MPL-2.0

use crate::battery::{BatteryDevice, BatteryMonitor};
use crate::media::{MediaMonitor, MultiPlayerState, PlayerId};
use crate::network::NetworkMonitor;
use crate::notifications::{Notification, NotificationMonitor};
use crate::storage::{DiskInfo, StorageMonitor};
use crate::temperature::TemperatureMonitor;
use crate::utilization::UtilizationMonitor;
use crate::weather::{WeatherData, WeatherMonitor};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct SystemSnapshot {
    pub cpu_usage: f32,
    pub memory_usage: f32,
    pub gpu_usage: f32,
    pub network_rx_rate: f64,
    pub network_tx_rate: f64,
    pub cpu_temp: f32,
    pub gpu_temp: f32,
    pub disks: Vec<DiskInfo>,
    pub devices: Vec<BatteryDevice>,
    pub weather: Option<WeatherData>,
    pub notifications: Vec<Notification>,
    pub media: MultiPlayerState,
}

#[derive(Clone)]
pub struct StatsSampler {
    latest: Arc<Mutex<SystemSnapshot>>,
    interval_ms: Arc<AtomicU64>,
    weather_enabled: Arc<AtomicBool>,
    weather_location: Arc<Mutex<String>>,
    notification_monitor: NotificationMonitor,
    media_monitor: MediaMonitor,
}

impl StatsSampler {
    pub fn spawn(
        interval_ms: u64,
        weather_enabled: bool,
        weather_location: String,
        max_notifications: usize,
        cider_api_token: String,
    ) -> Self {
        let notification_monitor = NotificationMonitor::new(max_notifications);
        let media_monitor = MediaMonitor::new(Some(cider_api_token));
        let sampler = Self {
            latest: Arc::new(Mutex::new(SystemSnapshot::default())),
            interval_ms: Arc::new(AtomicU64::new(interval_ms.max(250))),
            weather_enabled: Arc::new(AtomicBool::new(weather_enabled)),
            weather_location: Arc::new(Mutex::new(weather_location)),
            notification_monitor: notification_monitor.clone(),
            media_monitor: media_monitor.clone(),
        };

        let latest = Arc::clone(&sampler.latest);
        let interval = Arc::clone(&sampler.interval_ms);
        let weather_enabled = Arc::clone(&sampler.weather_enabled);
        let weather_location = Arc::clone(&sampler.weather_location);
        let media_monitor = sampler.media_monitor.clone();
        std::thread::spawn(move || {
            let mut utilization = UtilizationMonitor::new();
            let mut network = NetworkMonitor::new();
            let mut temperature = TemperatureMonitor::new();
            let mut storage = StorageMonitor::new();
            let mut battery = BatteryMonitor::new();
            let mut active_weather_location = match weather_location.lock() {
                Ok(location) => location.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            };
            let mut weather = WeatherMonitor::new(String::new(), active_weather_location.clone());

            loop {
                utilization.update();
                network.update();
                temperature.update();
                storage.update();
                battery.update();

                let configured_location = match weather_location.lock() {
                    Ok(location) => location.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                if configured_location != active_weather_location {
                    weather.set_location(configured_location.clone());
                    active_weather_location = configured_location;
                }
                if weather_enabled.load(Ordering::Relaxed) {
                    weather.update();
                }

                let weather_data = match weather.weather_data.lock() {
                    Ok(data) => data.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };

                let snapshot = SystemSnapshot {
                    cpu_usage: utilization.cpu_usage,
                    memory_usage: utilization.memory_usage,
                    gpu_usage: utilization.get_gpu_usage(),
                    network_rx_rate: network.network_rx_rate,
                    network_tx_rate: network.network_tx_rate,
                    cpu_temp: temperature.cpu_temp,
                    gpu_temp: temperature.gpu_temp,
                    disks: storage.disk_info.clone(),
                    devices: battery.devices(),
                    weather: weather_data,
                    notifications: notification_monitor.get_notifications(),
                    media: media_monitor.get_player_state(),
                };

                match latest.lock() {
                    Ok(mut current) => *current = snapshot,
                    Err(poisoned) => *poisoned.into_inner() = snapshot,
                }

                std::thread::sleep(Duration::from_millis(
                    interval.load(Ordering::Relaxed).max(250),
                ));
            }
        });

        sampler
    }

    pub fn snapshot(&self) -> SystemSnapshot {
        let mut snapshot = match self.latest.lock() {
            Ok(snapshot) => snapshot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        snapshot.notifications = self.notification_monitor.get_notifications();
        snapshot.media = self.media_monitor.get_player_state();
        snapshot
    }

    pub fn set_interval(&self, interval_ms: u64) {
        self.interval_ms
            .store(interval_ms.max(250), Ordering::Relaxed);
    }

    pub fn set_weather_config(&self, enabled: bool, location: String) {
        self.weather_enabled.store(enabled, Ordering::Relaxed);
        match self.weather_location.lock() {
            Ok(mut current) => *current = location,
            Err(poisoned) => *poisoned.into_inner() = location,
        }
    }

    pub fn clear_notifications(&self) {
        self.notification_monitor.clear();
    }

    pub fn dismiss_notification(&self, app_name: &str, timestamp: u64) {
        self.notification_monitor
            .remove_notification(app_name, timestamp);
    }

    pub fn set_cider_token(&self, token: String) {
        self.media_monitor
            .set_cider_token((!token.is_empty()).then_some(token));
    }

    pub fn media_state(&self) -> MultiPlayerState {
        self.media_monitor.get_player_state()
    }

    pub fn play_pause_media(&self) {
        self.media_monitor.play_pause();
    }

    pub fn previous_media(&self) {
        self.media_monitor.previous();
    }

    pub fn next_media(&self) {
        self.media_monitor.next();
    }

    pub fn select_media_player(&self, player_id: &PlayerId) {
        self.media_monitor.select_player_by_id(player_id);
    }

    pub fn seek_media(&self, progress: f64) {
        self.media_monitor.seek_to_progress(progress);
    }
}
