// SPDX-License-Identifier: MPL-2.0

use super::{
    BatteryDevice, BatteryMonitor, ExternalDeviceState, ExternalProbePlan,
    INITIAL_NATIVE_POLL_INTERVAL, INITIAL_PROBE_TIMEOUT, LOGITECH_POLL_INTERVAL,
    NATIVE_POLL_INTERVAL, WOLVERINE_DEVICE_NAME, expire_initial_readings, external_probe_plan,
    headsets, merge_native_headsets, merge_native_logitech, merge_native_maxwell,
    merge_native_wolverine, native_poll_interval, parse_headsetcontrol_json, parse_solaar_json,
    parse_solaar_text, prepare_detected_devices, reconcile_external_fallbacks,
    reconcile_native_headset_fallbacks,
};
use std::sync::{Arc, Mutex, atomic::AtomicBool};
use std::time::{Duration, Instant};

fn battery_device(name: &str, loading: bool) -> BatteryDevice {
    BatteryDevice {
        name: name.to_string(),
        level: None,
        status: None,
        kind: None,
        codename: None,
        is_loading: loading,
        is_connected: false,
    }
}

fn monitor_with_snapshot(devices: Vec<BatteryDevice>) -> BatteryMonitor {
    BatteryMonitor {
        devices: Arc::new(Mutex::new(devices)),
        logitech_devices: Arc::new(Mutex::new(Vec::new())),
        native_headset_coverage: Arc::new(Mutex::new(Vec::new())),
        cached_devices: Vec::new(),
        initial_probe_started: Instant::now(),
        last_update: Instant::now(),
        refresh_interval: super::EXTERNAL_FALLBACK_REFRESH_INTERVAL,
        update_requested: Arc::new(Mutex::new(false)),
        solaar_enabled: Arc::new(AtomicBool::new(true)),
    }
}

#[test]
fn charging_updates_do_not_wait_for_slow_backend_snapshots() {
    let discharging = BatteryDevice {
        level: Some(20),
        status: Some("discharging".to_string()),
        is_connected: true,
        ..battery_device("MX Mechanical Mini", false)
    };
    let headset = BatteryDevice {
        level: Some(80),
        is_connected: true,
        ..battery_device("Audeze Maxwell", false)
    };
    let slow_snapshot = vec![discharging.clone(), headset.clone()];
    let monitor = monitor_with_snapshot(slow_snapshot.clone());

    assert_eq!(monitor.devices(), slow_snapshot);
    let charging = BatteryDevice {
        status: Some("charging".to_string()),
        ..discharging.clone()
    };
    // A native update arrives while the slower coordinator is still busy.
    *monitor.logitech_devices.lock().unwrap() = vec![charging.clone()];
    assert_eq!(monitor.devices(), vec![charging.clone(), headset.clone()]);

    // The slow query finishes with an older value. It must not undo charging,
    // even when the percentage has not changed.
    *monitor.devices.lock().unwrap() = slow_snapshot;
    assert_eq!(monitor.devices(), vec![charging, headset.clone()]);

    *monitor.logitech_devices.lock().unwrap() = vec![discharging.clone()];
    assert_eq!(
        monitor.devices(),
        vec![discharging.clone(), headset.clone()]
    );

    *monitor.logitech_devices.lock().unwrap() = vec![BatteryDevice {
        is_connected: false,
        ..discharging
    }];
    assert_eq!(monitor.devices(), vec![headset]);
}

#[test]
fn logitech_updates_preserve_dedicated_headset_ownership() {
    let monitor = monitor_with_snapshot(Vec::new());
    let generic_headset = BatteryDevice {
        level: Some(70),
        is_connected: true,
        ..battery_device("Logitech G522", false)
    };
    let dedicated_headset = BatteryDevice {
        level: Some(80),
        ..generic_headset.clone()
    };
    *monitor.logitech_devices.lock().unwrap() = vec![generic_headset.clone()];
    *monitor.native_headset_coverage.lock().unwrap() = vec!["G522".to_string()];
    *monitor.devices.lock().unwrap() = vec![dedicated_headset.clone()];
    super::merge_uncovered_native_logitech(
        &mut monitor.devices.lock().unwrap(),
        std::slice::from_ref(&generic_headset),
        &["G522".to_string()],
    );
    assert_eq!(monitor.devices(), vec![dedicated_headset]);

    // Dedicated headset readers also own disconnected states, represented
    // by coverage without a visible row.
    monitor.devices.lock().unwrap().clear();
    super::merge_uncovered_native_logitech(
        &mut monitor.devices.lock().unwrap(),
        &[generic_headset],
        &["G522".to_string()],
    );
    assert!(monitor.devices().is_empty());
}

fn headsetcontrol_output(status: &str, level: i64) -> String {
    format!(
        r#"{{
            "devices": [{{
                "status": "success",
                "device": "Test Headset",
                "battery": {{"status": "{status}", "level": {level}}}
            }}]
        }}"#
    )
}

#[test]
fn maps_headsetcontrol_charging_status() {
    let devices =
        parse_headsetcontrol_json(&headsetcontrol_output("BATTERY_CHARGING", 98)).unwrap();

    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].level, Some(98));
    assert_eq!(devices[0].status.as_deref(), Some("charging"));
}

#[test]
fn maps_headsetcontrol_available_status_to_discharging() {
    let devices =
        parse_headsetcontrol_json(&headsetcontrol_output("BATTERY_AVAILABLE", 73)).unwrap();

    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].level, Some(73));
    assert_eq!(devices[0].status.as_deref(), Some("discharging"));
}

#[test]
fn solaar_json_rejects_malformed_binary_device_names() {
    let output = r#"[
        {
            "name": "\u0000\u0001\u0005\u0016",
            "battery": {
                "level": 70,
                "status": "BatteryStatus.DISCHARGING"
            }
        },
        {
            "name": "MX Mechanical Mini",
            "kind": "keyboard",
            "battery": {"level": 65, "status": "discharging"}
        }
    ]"#;

    let devices = parse_solaar_json(output).unwrap();

    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "MX Mechanical Mini");
}

#[test]
fn solaar_text_rejects_symbol_only_device_names() {
    let output = "  1: !@#$%^&*\n        Battery: 70% (discharging)\n";

    assert!(parse_solaar_text(output).is_empty());
}

#[test]
fn startup_polling_is_fast_only_while_a_cached_reading_is_unresolved() {
    let mut devices = vec![battery_device("Audeze Maxwell", true)];

    assert_eq!(
        native_poll_interval(&devices, Duration::from_secs(2), false),
        INITIAL_NATIVE_POLL_INTERVAL
    );

    devices[0].is_loading = false;
    assert_eq!(
        native_poll_interval(&devices, Duration::from_secs(2), false),
        NATIVE_POLL_INTERVAL
    );

    assert_eq!(
        native_poll_interval(&devices, Duration::from_secs(2), true),
        LOGITECH_POLL_INTERVAL
    );
}

#[test]
fn unresolved_connected_readings_become_unavailable_after_the_timeout() {
    let mut devices = vec![BatteryDevice {
        level: Some(80),
        status: Some("charging".to_string()),
        is_loading: true,
        is_connected: true,
        ..battery_device("Audeze Maxwell", true)
    }];

    expire_initial_readings(&mut devices, INITIAL_PROBE_TIMEOUT);

    assert_eq!(devices[0].level, None);
    assert_eq!(devices[0].status, None);
    assert!(!devices[0].is_loading);
    assert!(devices[0].is_connected);
    assert_eq!(
        native_poll_interval(&devices, INITIAL_PROBE_TIMEOUT, false),
        NATIVE_POLL_INTERVAL
    );
}

#[test]
fn cached_readings_are_applied_only_to_detected_connected_devices() {
    let cached = vec![
        BatteryDevice {
            level: Some(82),
            status: Some("discharging".to_string()),
            ..battery_device("Audeze Maxwell", true)
        },
        BatteryDevice {
            level: Some(100),
            status: Some("discharging".to_string()),
            ..battery_device("G309 LIGHTSPEED", true)
        },
    ];
    let mut detected = vec![
        BatteryDevice {
            is_connected: false,
            ..battery_device("Audeze Maxwell", false)
        },
        BatteryDevice {
            is_connected: true,
            ..battery_device("G309 LIGHTSPEED", false)
        },
    ];

    prepare_detected_devices(&mut detected, &cached, true);

    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0].name, "G309 LIGHTSPEED");
    assert_eq!(detected[0].level, Some(100));
    assert!(detected[0].is_loading);
    assert!(detected[0].is_connected);
}

#[test]
fn live_battery_readings_take_precedence_over_cached_values() {
    let cached = vec![BatteryDevice {
        level: Some(65),
        ..battery_device("MX Mechanical Mini", true)
    }];
    let mut detected = vec![BatteryDevice {
        level: Some(60),
        is_connected: true,
        ..battery_device("MX Mechanical Mini", false)
    }];

    prepare_detected_devices(&mut detected, &cached, true);

    assert_eq!(detected[0].level, Some(60));
    assert!(!detected[0].is_loading);
}

#[test]
fn native_maxwell_replaces_the_cli_copy() {
    let mut devices = vec![
        BatteryDevice {
            name: "G309 LIGHTSPEED".to_string(),
            level: Some(100),
            status: Some("discharging".to_string()),
            kind: Some("mouse".to_string()),
            codename: None,
            is_loading: false,
            is_connected: true,
        },
        BatteryDevice {
            name: "Audeze Maxwell".to_string(),
            level: Some(25),
            status: Some("discharging".to_string()),
            kind: Some("headset".to_string()),
            codename: None,
            is_loading: false,
            is_connected: true,
        },
    ];
    let native = BatteryDevice {
        level: Some(96),
        status: Some("charging".to_string()),
        ..devices[1].clone()
    };

    merge_native_maxwell(&mut devices, Some(native));

    assert_eq!(devices.len(), 2);
    assert_eq!(devices[1].level, Some(96));
    assert_eq!(devices[1].status.as_deref(), Some("charging"));
    assert_eq!(devices[0].name, "G309 LIGHTSPEED");
}

#[test]
fn disconnected_native_maxwell_removes_the_cached_row() {
    let mut devices = vec![BatteryDevice {
        name: "Audeze Maxwell".to_string(),
        level: None,
        status: None,
        kind: Some("headset".to_string()),
        codename: None,
        is_loading: false,
        is_connected: false,
    }];
    let disconnected = devices[0].clone();

    merge_native_maxwell(&mut devices, Some(disconnected));

    assert!(devices.is_empty());
}

#[test]
fn wolverine_without_a_live_battery_reading_is_hidden() {
    let controller = BatteryDevice {
        name: WOLVERINE_DEVICE_NAME.to_string(),
        level: Some(80),
        status: Some("discharging".to_string()),
        kind: Some("controller".to_string()),
        codename: None,
        is_loading: false,
        is_connected: true,
    };

    for unavailable in [
        BatteryDevice {
            level: None,
            is_connected: false,
            ..controller.clone()
        },
        BatteryDevice {
            level: None,
            is_connected: true,
            ..controller.clone()
        },
    ] {
        let mut devices = vec![controller.clone()];
        merge_native_wolverine(&mut devices, Some(unavailable));
        assert!(devices.is_empty());
    }
}

#[test]
fn wolverine_with_a_battery_reading_is_visible() {
    let controller = BatteryDevice {
        name: WOLVERINE_DEVICE_NAME.to_string(),
        level: Some(80),
        status: Some("discharging".to_string()),
        kind: Some("controller".to_string()),
        codename: None,
        is_loading: false,
        is_connected: true,
    };
    let mut devices = Vec::new();

    merge_native_wolverine(&mut devices, Some(controller));

    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].level, Some(80));
}

#[test]
fn native_logitech_replaces_matching_solaar_devices_only() {
    let mut devices = vec![
        BatteryDevice {
            name: "Logitech G309 LIGHTSPEED".to_string(),
            level: Some(50),
            status: Some("discharging".to_string()),
            kind: Some("mouse".to_string()),
            codename: None,
            is_loading: false,
            is_connected: true,
        },
        BatteryDevice {
            name: "Unsupported Logitech device".to_string(),
            level: Some(75),
            status: Some("discharging".to_string()),
            kind: Some("mouse".to_string()),
            codename: None,
            is_loading: false,
            is_connected: true,
        },
    ];
    let native = BatteryDevice {
        name: "G309".to_string(),
        level: Some(100),
        ..devices[0].clone()
    };

    merge_native_logitech(&mut devices, &[native]);

    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].name, "Logitech G309 LIGHTSPEED");
    assert_eq!(devices[1].name, "Unsupported Logitech device");
    assert_eq!(
        devices
            .iter()
            .find(|device| device.name == "Logitech G309 LIGHTSPEED")
            .and_then(|device| device.level),
        Some(100)
    );
    assert!(
        devices
            .iter()
            .any(|device| device.name == "Unsupported Logitech device")
    );
}

#[test]
fn native_logitech_collapses_all_cached_aliases() {
    let template = BatteryDevice {
        name: String::new(),
        level: Some(50),
        status: Some("discharging".to_string()),
        kind: Some("mouse".to_string()),
        codename: None,
        is_loading: true,
        is_connected: false,
    };
    let mut devices = vec![
        BatteryDevice {
            name: "G309".to_string(),
            ..template.clone()
        },
        BatteryDevice {
            name: "G309 LIGHTSPEED".to_string(),
            ..template.clone()
        },
    ];
    let native = BatteryDevice {
        name: "G309 LIGHTSPEED".to_string(),
        level: Some(100),
        is_loading: false,
        is_connected: true,
        ..template
    };

    merge_native_logitech(&mut devices, &[native]);

    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "G309 LIGHTSPEED");
    assert_eq!(devices[0].level, Some(100));
    assert!(!devices[0].is_loading);
    assert!(devices[0].is_connected);
}

#[test]
fn disconnected_native_logitech_removes_the_cached_row() {
    let mut devices = vec![BatteryDevice {
        name: "G309 LIGHTSPEED".to_string(),
        level: Some(100),
        status: Some("discharging".to_string()),
        kind: Some("mouse".to_string()),
        codename: None,
        is_loading: true,
        is_connected: true,
    }];
    let disconnected = BatteryDevice {
        level: None,
        status: None,
        is_loading: false,
        is_connected: false,
        ..devices[0].clone()
    };

    merge_native_logitech(&mut devices, &[disconnected]);

    assert!(devices.is_empty());
}

#[test]
fn native_headset_replaces_the_headsetcontrol_copy() {
    let mut devices = vec![BatteryDevice {
        name: "SteelSeries Arctis Nova 7".to_string(),
        level: Some(25),
        status: Some("discharging".to_string()),
        kind: Some("headset".to_string()),
        codename: None,
        is_loading: false,
        is_connected: true,
    }];
    let native = headsets::BatteryState {
        name: "SteelSeries Arctis Nova 7".to_string(),
        level: Some(75),
        status: Some("charging".to_string()),
    };

    merge_native_headsets(
        &mut devices,
        &[native],
        &["SteelSeries Arctis Nova 7".to_string()],
    );

    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].level, Some(75));
    assert_eq!(devices[0].status.as_deref(), Some("charging"));
    assert_eq!(devices[0].kind.as_deref(), Some("headset"));
}

#[test]
fn powered_off_native_headset_removes_stale_rows_and_cli_polling() {
    let name = "SteelSeries Arctis Nova 7".to_string();
    let mut devices = vec![battery_device(&name, false)];
    let mut external = ExternalDeviceState {
        headsetcontrol_devices: devices.clone(),
        ..Default::default()
    };
    external
        .headsetcontrol_fallback_names
        .insert(name.to_ascii_lowercase());

    merge_native_headsets(&mut devices, &[], std::slice::from_ref(&name));
    reconcile_native_headset_fallbacks(&mut external, std::slice::from_ref(&name));

    assert!(devices.is_empty());
    assert!(external.headsetcontrol_fallback_names.is_empty());
}

#[test]
fn native_logitech_headset_replaces_a_shorter_discovered_name() {
    let mut devices = vec![battery_device("G522", false)];
    let native = headsets::BatteryState {
        name: "Logitech G522 LIGHTSPEED".to_string(),
        level: Some(90),
        status: Some("discharging".to_string()),
    };

    merge_native_headsets(
        &mut devices,
        &[native],
        &["Logitech G522 LIGHTSPEED".to_string()],
    );

    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "Logitech G522 LIGHTSPEED");
    assert_eq!(devices[0].level, Some(90));
}

#[test]
fn native_devices_disable_thirty_second_external_polling() {
    let native_logitech = vec![battery_device("G309 LIGHTSPEED", false)];
    let mut external = ExternalDeviceState {
        solaar_devices: vec![battery_device("G309 LIGHTSPEED", false)],
        headsetcontrol_devices: vec![battery_device("Audeze Maxwell", false)],
        ..Default::default()
    };

    reconcile_external_fallbacks(&mut external, true, &native_logitech);

    assert_eq!(
        external_probe_plan(&external, false, true, true),
        ExternalProbePlan {
            solaar: false,
            headsetcontrol: false,
        }
    );
    assert_eq!(
        external_probe_plan(&external, true, true, true),
        ExternalProbePlan::DISCOVERY
    );
}

#[test]
fn unsupported_devices_keep_only_their_fallback_backend_active() {
    let native_logitech = vec![battery_device("G309 LIGHTSPEED", false)];
    let mut external = ExternalDeviceState {
        solaar_devices: vec![
            battery_device("G309 LIGHTSPEED", false),
            battery_device("Unsupported Logitech device", false),
        ],
        headsetcontrol_devices: vec![battery_device("Audeze Maxwell", false)],
        ..Default::default()
    };

    reconcile_external_fallbacks(&mut external, true, &native_logitech);

    assert_eq!(
        external_probe_plan(&external, false, true, true),
        ExternalProbePlan {
            solaar: true,
            headsetcontrol: false,
        }
    );
}

#[test]
fn native_coverage_retires_a_temporary_solaar_fallback() {
    let mut external = ExternalDeviceState::default();
    external
        .solaar_fallback_names
        .insert("mx mechanical mini".to_string());
    assert!(external_probe_plan(&external, false, true, true).solaar);

    reconcile_external_fallbacks(
        &mut external,
        true,
        &[battery_device("MX Mechanical Mini", false)],
    );

    assert!(!external_probe_plan(&external, false, true, true).solaar);
}

#[test]
fn headsetcontrol_remains_a_fallback_when_native_maxwell_fails() {
    let external = ExternalDeviceState::default();

    assert_eq!(
        external_probe_plan(&external, false, false, true),
        ExternalProbePlan {
            solaar: false,
            headsetcontrol: true,
        }
    );
}

#[test]
fn disabled_solaar_never_enters_the_probe_plan() {
    let mut external = ExternalDeviceState::default();
    external
        .solaar_fallback_names
        .insert("unsupported logitech device".to_string());

    assert!(!external_probe_plan(&external, true, true, false).solaar);
}
