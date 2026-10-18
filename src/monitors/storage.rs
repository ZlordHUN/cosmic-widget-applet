// SPDX-License-Identifier: MPL-2.0

//! # Storage/Disk Monitoring Module
//!
//! This module monitors disk usage across mounted filesystems, providing
//! space utilization and friendly device names from sysfs.
//!
//! ## Features
//!
//! - **Disk usage tracking**: Total, available, and used space percentages
//! - **Smart filtering**: Only shows meaningful mounts (/, /home, external drives)
//! - **Friendly names**: Reads vendor/model names from sysfs instead of device paths
//! - **Caching**: Shows cached disk list immediately while loading real data
//! - **Background updates**: Disk model fetching runs in a separate thread
//!
//! ## Mount Point Filtering
//!
//! To avoid showing system partitions, the module only displays:
//! - **Root (`/`)**: Main system partition
//! - **Home (`/home`)**: User data partition
//! - **External mounts (`/mnt/*`, `/media/*`)**: USB drives, network shares
//!
//! Filtered out:
//! - `/boot`, `/snap`, `/run`, `/sys`, `/proc`, `/dev`, `/tmp`, `/var/snap`
//!
//! ## Device Name Resolution
//!
//! ```text
//! /dev/nvme0n1p1 → "Samsung 970 EVO"  (via sysfs)
//! /dev/sda1      → "WDC WD10EZEX"     (via sysfs)
//! /home          → "Home"              (hardcoded)
//! /              → "System" or model   (fallback)
//! ```
//!
//! ## Architecture
//!
//! - Main thread: Calls `update()` to refresh disk space from sysinfo
//! - Background thread: Reads sysfs every 10 seconds to update model names
//! - Shared state: `disk_models` HashMap protected by Arc<Mutex>

use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::fs;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use sysinfo::Disks;

// ============================================================================
// Disk Information Struct
// ============================================================================

/// Information about a single mounted disk/partition.
///
/// This struct holds all data needed to display a disk in the widget,
/// including human-readable names and usage statistics.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskInfo {
    /// Display name for the disk (model name, "Home", "System", or mount name)
    pub name: String,
    /// Mount point path (e.g., "/", "/home", "/mnt/usb")
    pub mount_point: String,
    /// Percentage of disk space used (0.0 - 100.0)
    pub used_percentage: f32,
    /// Total disk capacity in bytes
    pub total_space: u64,
    /// Available free space in bytes
    pub available_space: u64,
    /// True if showing cached data while loading real data
    pub is_loading: bool,
}

// ============================================================================
// Storage Monitor Struct
// ============================================================================

/// Monitors disk usage across mounted filesystems.
///
/// Uses sysinfo for disk space queries and sysfs for device model names.
/// The monitor maintains a list of relevant disks with friendly display names.
///
/// # Architecture
///
/// - **Disk models**: Fetched by background thread every 10 seconds via sysfs
/// - **Disk space**: Refreshed in main thread via sysinfo on each `update()`
/// - **Caching**: Shows cached disk list on startup for instant display
///
/// # Thread Safety
///
/// `disk_models` is wrapped in `Arc<Mutex>` for safe access between the
/// main update thread and the background model-fetching thread.
pub struct StorageMonitor {
    /// sysinfo's disk list (refreshed on update)
    disks: Disks,
    /// One representative mount per local filesystem, rebuilt with the disk list.
    display_disk_indices: Vec<usize>,
    /// List of filtered disk information for display
    pub disk_info: Vec<DiskInfo>,
    /// Map of device name → model name (e.g., "nvme0n1" → "Samsung 970 EVO")
    /// Updated by the background thread from sysfs
    disk_models: Arc<Mutex<HashMap<String, String>>>,
    /// Remote storage discovered through desktop mounts such as GVFS SFTP/SMB.
    /// Updated in the background because an unavailable network mount can be slow.
    remote_disks: Arc<Mutex<Vec<DiskInfo>>>,
    /// Flag to track first update for cache saving
    is_first_update: bool,
    /// Counter for periodic full disk list refresh (to detect new mounts)
    update_counter: u32,
}

impl StorageMonitor {
    /// Create a new storage monitor with background model fetching.
    ///
    /// # Initialization Steps
    ///
    /// 1. Load cached disk names for instant display
    /// 2. Initialize sysinfo disk list
    /// 3. Spawn background thread to fetch disk models from sysfs
    ///
    /// The background thread updates model names every 10 seconds since
    /// hardware rarely changes during runtime.
    pub fn new() -> Self {
        let disk_models = Arc::new(Mutex::new(HashMap::new()));
        let disks = Disks::new_with_refreshed_list();
        let display_disk_indices = display_disk_indices(&disks);

        // Load cached disk info to show immediately
        // This provides instant display while real data loads
        let cache = super::cache::WidgetCache::load();
        let disk_info: Vec<DiskInfo> = cache
            .disks
            .iter()
            // Discard cached duplicate bind mounts before the first live update.
            // GVFS connections are discovered separately in the background.
            .filter(|d| {
                d.mount_point.contains("/gvfs/")
                    || display_disk_indices.iter().any(|&index| {
                        disks.list()[index].mount_point() == Path::new(&d.mount_point)
                    })
            })
            .map(|d| DiskInfo {
                name: d.name.clone(),
                mount_point: d.mount_point.clone(),
                used_percentage: 0.0, // Will be updated on first refresh
                total_space: 0,
                available_space: 0,
                is_loading: true, // Mark as loading until real data arrives
            })
            .collect();
        let remote_disks = Arc::new(Mutex::new(
            disk_info
                .iter()
                .filter(|disk| disk.mount_point.contains("/gvfs/"))
                .cloned()
                .collect(),
        ));

        // Fetch local model names and remote filesystem capacity off the sampler
        // thread. A disconnected network filesystem can otherwise stall all stats.
        let disk_models_clone = Arc::clone(&disk_models);
        let remote_disks_clone = Arc::clone(&remote_disks);
        std::thread::spawn(move || {
            loop {
                if let Some(models) = Self::fetch_disk_models() {
                    *disk_models_clone.lock().unwrap() = models;
                }
                *remote_disks_clone.lock().unwrap() = Self::fetch_remote_disks();

                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });

        Self {
            disks,
            display_disk_indices,
            disk_info,
            disk_models,
            remote_disks,
            is_first_update: true,
            update_counter: 0,
        }
    }

    /// Fetch disk model names from sysfs (called from background thread).
    ///
    /// # Returns
    ///
    /// HashMap mapping device names to vendor+model strings:
    /// - Key: "nvme0n1", "sda", etc.
    /// - Value: "Samsung SSD 970", "WDC WD10EZEX", etc.
    ///
    /// # Example
    ///
    /// ```text
    /// /sys/class/block/nvme0n1/device/model: "Samsung SSD 970 EVO Plus"
    /// Result: {"nvme0n1" => "Samsung SSD 970 EVO Plus"}
    /// ```
    fn fetch_disk_models() -> Option<HashMap<String, String>> {
        Self::fetch_disk_models_from(Path::new("/sys/class/block"))
    }

    fn fetch_disk_models_from(block_dir: &Path) -> Option<HashMap<String, String>> {
        let mut models = HashMap::new();

        for entry in fs::read_dir(block_dir).ok()?.filter_map(Result::ok) {
            let Some(device) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let device_dir = entry.path().join("device");
            let vendor = read_sysfs_text(&device_dir.join("vendor"));
            let model = read_sysfs_text(&device_dir.join("model"));
            let display_name = [vendor, model]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");

            if !display_name.is_empty() {
                models.insert(device, display_name);
            }
        }

        Some(models)
    }

    /// Discover remote filesystems mounted by the desktop through GVFS.
    ///
    /// `sysinfo` exposes the GVFS FUSE container rather than the individual
    /// SFTP/SMB connections, so each connection directory must be queried.
    fn fetch_remote_disks() -> Vec<DiskInfo> {
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })));
        let gvfs_dir = runtime_dir.join("gvfs");
        let Ok(entries) = fs::read_dir(gvfs_dir) else {
            return Vec::new();
        };

        entries
            .filter_map(Result::ok)
            .filter_map(|entry| Self::remote_disk_from_mount(&entry.path()))
            .collect()
    }

    fn remote_disk_from_mount(mount_path: &Path) -> Option<DiskInfo> {
        let mount_name = mount_path.file_name()?.to_str()?;
        if !is_gvfs_remote_mount(mount_name) {
            return None;
        }

        let qnap_data_path = qnap_data_path(mount_path);
        let stats_path = qnap_data_path.as_deref().unwrap_or(mount_path);
        let (total_space, available_space) = filesystem_space(stats_path)?;
        if total_space == 0 {
            return None;
        }

        let used_percentage =
            ((total_space - available_space) as f64 / total_space as f64 * 100.0) as f32;

        Some(DiskInfo {
            name: remote_display_name(mount_name, qnap_data_path.is_some()),
            mount_point: stats_path.to_string_lossy().into_owned(),
            used_percentage,
            total_space,
            available_space,
            is_loading: false,
        })
    }

    /// Update disk information from sysinfo.
    ///
    /// Refreshes disk space data and rebuilds the filtered disk list with
    /// friendly display names from the model cache.
    ///
    /// # Processing Steps
    ///
    /// 1. Refresh sysinfo disk data (NOT disk list to avoid FD leaks)
    /// 2. Filter to only meaningful mount points
    /// 3. Calculate usage percentages
    /// 4. Map device names to friendly model names
    /// 5. Update cache on first successful refresh
    ///
    /// # Mount Point Filtering Rules
    ///
    /// **Included:**
    /// - `/` (root filesystem)
    /// - `/home` (user data)
    /// - `/mnt/*` and `/media/*` (external mounts)
    ///
    /// **Excluded:**
    /// - `/boot`, `/snap`, `/run`, `/sys`, `/proc`, `/dev`, `/tmp`, `/var/snap`
    ///
    /// # Device Name Extraction
    ///
    /// Extracts base device name for model lookup:
    /// - `/dev/nvme0n1p1` → `nvme0n1` (NVMe partition)
    /// - `/dev/sda1` → `sda` (SATA partition)
    /// - `/dev/mmcblk0p1` → `mmcblk0` (SD card partition)
    pub fn update(&mut self) {
        // Periodically refresh the full disk list to detect new mounts
        // Every 30 updates (~30 seconds with 1s interval) we rescan for new disks
        // This catches USB drives being plugged in or new partitions being mounted
        self.update_counter += 1;
        if self.update_counter >= 30 {
            self.update_counter = 0;
            self.disks = Disks::new_with_refreshed_list();
            self.display_disk_indices = display_disk_indices(&self.disks);
        } else {
            // Normal update: just refresh existing disk data (fast, no FD leak)
            self.disks.refresh();
        }

        self.disk_info.clear();

        // Get disk models from cache (updated by background thread)
        let disk_models = self.disk_models.lock().unwrap().clone();

        for &index in &self.display_disk_indices {
            let disk = &self.disks.list()[index];
            let mount_point = disk.mount_point().to_string_lossy().to_string();

            // ================================================================
            // Space Calculation
            // ================================================================
            let total = disk.total_space();
            let available = disk.available_space();
            let used = total - available;
            let used_percentage = if total > 0 {
                (used as f32 / total as f32) * 100.0
            } else {
                0.0
            };

            // ================================================================
            // Device Name Resolution
            // ================================================================
            // Get the device name (e.g., sda, nvme0n1, sdb)
            let device_name = disk.name().to_string_lossy().to_string();

            // Extract the base disk name (without partition number)
            // e.g., /dev/sda1 -> sda, /dev/nvme0n1p1 -> nvme0n1
            let base_device = if let Some(dev) = device_name.strip_prefix("/dev/") {
                // Remove partition numbers
                if dev.contains("nvme") || dev.contains("mmcblk") {
                    // NVMe or MMC devices: nvme0n1p1 -> nvme0n1
                    dev.split('p').next().unwrap_or(dev)
                } else {
                    // Regular devices: sda1 -> sda
                    dev.trim_end_matches(|c: char| c.is_ascii_digit())
                }
            } else {
                &device_name
            };

            // ================================================================
            // Display Name Resolution
            // ================================================================
            // Try to get a better label for the disk
            let display_name = if mount_point == "/" {
                // For root, try to get model name or use "System"
                disk_models
                    .get(base_device)
                    .map(|m| m.clone())
                    .unwrap_or_else(|| "System".to_string())
            } else if mount_point == "/home" {
                "Home".to_string()
            } else {
                // For external drives, try to get the model name
                disk_models
                    .get(base_device)
                    .map(|m| m.clone())
                    .unwrap_or_else(|| {
                        // Fallback to mount point name (e.g., "USB_Drive" from /mnt/USB_Drive)
                        mount_point
                            .split('/')
                            .last()
                            .unwrap_or(&mount_point)
                            .to_string()
                    })
            };

            self.disk_info.push(DiskInfo {
                name: display_name,
                mount_point,
                used_percentage,
                total_space: total,
                available_space: available,
                is_loading: false,
            });
        }

        for remote_disk in self.remote_disks.lock().unwrap().iter() {
            if !self
                .disk_info
                .iter()
                .any(|disk| disk.mount_point == remote_disk.mount_point)
            {
                self.disk_info.push(remote_disk.clone());
            }
        }

        // Update cache after first successful update
        // This saves disk names for instant display on next startup
        if self.is_first_update && !self.disk_info.is_empty() {
            let mut cache = super::cache::WidgetCache::load();
            cache.update_disks(&self.disk_info);
            self.is_first_update = false;
        }
    }
}

/// Resolve only local device nodes, never mount paths that might be remote.
/// rdev identifies the partition even through /dev/root or /dev/disk/by-* aliases.
fn block_device_id(source: &OsStr) -> Option<u64> {
    if !Path::new(source).starts_with("/dev") {
        return None;
    }
    let metadata = fs::metadata(source).ok()?;
    metadata
        .file_type()
        .is_block_device()
        .then(|| metadata.rdev())
}

fn is_display_mount(path: &Path) -> bool {
    path == Path::new("/")
        || path == Path::new("/home")
        || ["/mnt", "/media"]
            .iter()
            .any(|root| path.starts_with(root) && path != Path::new(root))
}

fn mount_preference(path: &Path) -> (u8, usize, &Path) {
    let priority = if path == Path::new("/") {
        0
    } else if path == Path::new("/home") {
        1
    } else {
        2
    };
    (priority, path.components().count(), path)
}

/// Preserve disk order, replacing a bind mount with the main mount if necessary.
/// Different partitions and same-model drives have distinct device IDs. Unknown
/// identities and non-block sources remain separate rather than hiding storage.
fn select_display_mounts<'a>(
    mounts: impl IntoIterator<Item = (&'a Path, Option<u64>)>,
) -> Vec<usize> {
    let mut selected: Vec<(usize, &Path)> = Vec::new();
    let mut filesystems: HashMap<u64, usize> = HashMap::new();
    for (index, (path, device_id)) in mounts.into_iter().enumerate() {
        if !is_display_mount(path) {
            continue;
        }
        if let Some(previous) = device_id.and_then(|id| filesystems.get(&id).copied()) {
            if mount_preference(path) < mount_preference(selected[previous].1) {
                selected[previous] = (index, path);
            }
            continue;
        }
        if let Some(id) = device_id {
            filesystems.insert(id, selected.len());
        }
        selected.push((index, path));
    }
    selected.into_iter().map(|(index, _)| index).collect()
}

fn display_disk_indices(disks: &Disks) -> Vec<usize> {
    // Device lookups happen only at startup and the existing 30-update rescan,
    // not on each space refresh. No additional polling or worker is needed.
    select_display_mounts(disks.list().iter().map(|disk| {
        let mount = disk.mount_point();
        let device_id = is_display_mount(mount)
            .then(|| block_device_id(disk.name()))
            .flatten();
        (mount, device_id)
    }))
}

fn read_sysfs_text(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let normalized = String::from_utf8_lossy(&bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn is_gvfs_remote_mount(name: &str) -> bool {
    ["sftp:", "smb-share:", "dav:", "davs:", "ftp:", "nfs:"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// QNAP SFTP connections expose the small system filesystem at their root.
/// Prefer the user-facing NAS share, then the primary data volume, so the
/// displayed capacity describes storage rather than the appliance OS.
fn qnap_data_path(mount_path: &Path) -> Option<PathBuf> {
    let share_dir = mount_path.join("share");
    let primary_volume = share_dir.join("CACHEDEV1_DATA");
    if !primary_volume.is_dir() {
        return None;
    }

    let nas_share = share_dir.join("NAS");
    if nas_share.is_dir() {
        Some(nas_share)
    } else {
        let public_share = share_dir.join("Public");
        Some(if public_share.is_dir() {
            public_share
        } else {
            primary_volume
        })
    }
}

fn remote_display_name(mount_name: &str, is_qnap: bool) -> String {
    let attributes = mount_name
        .split_once(':')
        .map(|(_, attributes)| attributes)
        .unwrap_or_default();
    let value = |key: &str| {
        attributes.split(',').find_map(|attribute| {
            let (attribute_key, value) = attribute.split_once('=')?;
            (attribute_key == key).then(|| {
                urlencoding::decode(value)
                    .map(|value| value.into_owned())
                    .unwrap_or_else(|_| value.to_string())
            })
        })
    };
    let host = value("host").or_else(|| value("server"));

    if is_qnap {
        return host
            .map(|host| format!("QNAP NAS  ({host})"))
            .unwrap_or_else(|| "QNAP NAS".to_string());
    }

    match (value("share"), host) {
        (Some(share), Some(host)) => format!("{share} ({host})"),
        (Some(share), None) => share,
        (None, Some(host)) => host,
        (None, None) => "Network storage".to_string(),
    }
}

fn filesystem_space(path: &Path) -> Option<(u64, u64)> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    let stats = unsafe { stats.assume_init() };
    let block_size = if stats.f_frsize > 0 {
        stats.f_frsize
    } else {
        stats.f_bsize
    } as u64;

    Some((
        (stats.f_blocks as u64).saturating_mul(block_size),
        (stats.f_bavail as u64).saturating_mul(block_size),
    ))
}

#[cfg(test)]
mod tests {
    use super::{StorageMonitor, is_gvfs_remote_mount, remote_display_name, select_display_mounts};
    use std::fs;
    use std::path::Path;

    fn displayed_mounts<'a>(mounts: &[(&'a str, Option<u64>)]) -> Vec<&'a str> {
        select_display_mounts(mounts.iter().map(|(path, id)| (Path::new(path), *id)))
            .into_iter()
            .map(|index| mounts[index].0)
            .collect()
    }

    #[test]
    fn steam_bind_mount_shares_the_system_filesystem() {
        let bind = "/mnt/Games/SteamLibrary/steamapps/compatdata";
        assert_eq!(
            displayed_mounts(&[("/", Some(1)), ("/mnt/Games", Some(2)), (bind, Some(1))]),
            ["/", "/mnt/Games"]
        );
        // Enumeration order must not replace the system disk with its bind mount.
        assert_eq!(
            displayed_mounts(&[(bind, Some(1)), ("/mnt/Games", Some(2)), ("/", Some(1))]),
            ["/", "/mnt/Games"]
        );
    }

    #[test]
    fn prefers_home_then_shallow_stable_mounts_for_duplicate_filesystems() {
        assert_eq!(
            displayed_mounts(&[
                ("/mnt/backup/home", Some(1)),
                ("/home", Some(1)),
                ("/mnt/z-drive/folder", Some(2)),
                ("/media/z-drive", Some(2)),
                ("/media/a-drive", Some(2)),
            ]),
            ["/home", "/media/a-drive"]
        );
    }

    #[test]
    fn distinct_partitions_and_unresolved_sources_stay_visible() {
        // Identity comes from the full partition device, never the model name
        // or parent disk. Remote and unresolved sources are not conflated.
        assert_eq!(
            displayed_mounts(&[
                ("/", Some(1)),
                ("/home", Some(2)),
                ("/mnt/second-kingston", Some(3)),
                ("/mnt/network-a", None),
                ("/mnt/network-b", None),
                ("/media/unknown", None),
            ]),
            [
                "/",
                "/home",
                "/mnt/second-kingston",
                "/mnt/network-a",
                "/mnt/network-b",
                "/media/unknown"
            ]
        );
    }

    #[test]
    fn excluded_mounts_do_not_hide_visible_filesystems() {
        assert_eq!(
            displayed_mounts(&[
                ("/boot/efi", Some(1)),
                ("/mnt/visible", Some(1)),
                ("/mnt", Some(2)),
                ("/mnt-other", Some(3)),
                ("/run/media/hidden", Some(4)),
            ]),
            ["/mnt/visible"]
        );
    }

    #[test]
    fn recognizes_supported_gvfs_network_mounts() {
        assert!(is_gvfs_remote_mount("sftp:host=192.0.2.9"));
        assert!(is_gvfs_remote_mount(
            "smb-share:server=qnap.local,share=Media"
        ));
        assert!(!is_gvfs_remote_mount("mtp:host=phone"));
    }

    #[test]
    fn names_qnap_and_smb_storage() {
        assert_eq!(
            remote_display_name("sftp:host=192.0.2.9", true),
            "QNAP NAS  (192.0.2.9)"
        );
        assert_eq!(
            remote_display_name("smb-share:server=qnap.local,share=Media", false),
            "Media (qnap.local)"
        );
    }

    #[test]
    fn reads_and_normalizes_disk_models_from_sysfs() {
        let sysfs = std::env::temp_dir().join(format!(
            "cosmic-widget-storage-sysfs-{}",
            std::process::id()
        ));
        let nvme_device = sysfs.join("nvme0n1/device");
        let usb_device = sysfs.join("sda/device");
        fs::create_dir_all(&nvme_device).unwrap();
        fs::create_dir_all(&usb_device).unwrap();
        fs::create_dir_all(sysfs.join("loop0")).unwrap();
        fs::write(
            nvme_device.join("model"),
            b"Samsung SSD 990 EVO Plus 4TB            \n",
        )
        .unwrap();
        fs::write(usb_device.join("vendor"), b"Samsung \n").unwrap();
        fs::write(usb_device.join("model"), b"PSSD   T7         \n").unwrap();

        let models = StorageMonitor::fetch_disk_models_from(&sysfs).unwrap();

        assert_eq!(
            models.get("nvme0n1").map(String::as_str),
            Some("Samsung SSD 990 EVO Plus 4TB")
        );
        assert_eq!(
            models.get("sda").map(String::as_str),
            Some("Samsung PSSD T7")
        );
        assert!(!models.contains_key("loop0"));
        fs::remove_dir_all(sysfs).unwrap();
    }
}
