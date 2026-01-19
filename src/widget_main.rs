// SPDX-License-Identifier: MPL-2.0

//! COSMIC Monitor Widget - Standalone Desktop Widget
//!
//! This is the main entry point for the desktop monitoring widget. Unlike the
//! panel applet, this widget uses Wayland's layer-shell protocol to render
//! directly on the desktop, bypassing normal window management.
//!
//! # Binary
//!
//! Compiles to `cosmic-widget`, typically installed to `/usr/bin/`.
//! Can be launched via:
//! - Panel applet "Show Widget" button
//! - Auto-start when applet loads (if configured)
//! - Direct command line invocation
//!
//! # Architecture
//!
//! The widget uses smithay-client-toolkit to interact with Wayland:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │                        MonitorWidget                             │
//! ├──────────────────────────────────────────────────────────────────┤
//! │  Wayland State                                                   │
//! │  ├── LayerShell        (for desktop overlay positioning)        │
//! │  ├── CompositorState   (surface management)                     │
//! │  ├── ShmHandler        (shared memory buffers for rendering)    │
//! │  └── SeatState         (input handling: mouse, keyboard)        │
//! ├──────────────────────────────────────────────────────────────────┤
//! │  Monitor Modules                                                 │
//! │  ├── UtilizationMonitor  (CPU, Memory, GPU usage)               │
//! │  ├── TemperatureMonitor  (CPU/GPU temps from hwmon/nvidia-smi)  │
//! │  ├── StorageMonitor      (disk space from mount points)         │
//! │  ├── BatteryMonitor      (system + Solaar Bluetooth devices)    │
//! │  ├── WeatherMonitor      (OpenWeatherMap API)                   │
//! │  ├── NotificationMonitor (D-Bus notifications)                  │
//! │  └── MediaMonitor        (Cider Apple Music client)             │
//! └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Event Loop
//!
//! The main loop:
//! 1. Polls Wayland for events (input, configure, etc.)
//! 2. Updates system statistics at the configured interval
//! 3. Re-renders when the clock second changes
//! 4. Handles click events for notifications and media controls
//! 5. Checks for configuration changes every 500ms
//!
//! # Layer Shell
//!
//! The widget uses wlr-layer-shell to:
//! - Position at an absolute X,Y coordinate on the desktop
//! - Stay below regular windows (Layer::Bottom) - acts like desktop widget
//! - Not reserve exclusive space (other windows can overlap)
//! - Accept mouse input for dragging (when settings is open) and clicks
//!
//! # Reconnection
//!
//! If the Wayland connection is lost (compositor restart, etc.), the widget
//! automatically attempts to reconnect with exponential backoff.

mod config;
mod widget;

use config::Config;
use widget::{UtilizationMonitor, TemperatureMonitor, NetworkMonitor, WeatherMonitor, StorageMonitor, BatteryMonitor, NotificationMonitor, MediaMonitor, CosmicTheme, load_weather_font};
use widget::renderer::{render_widget, RenderParams};
use widget::layout::calculate_widget_height_with_all;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::thread;

// smithay-client-toolkit provides Rust-friendly wrappers around Wayland protocols
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    delegate_seat, delegate_pointer,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{Capability, SeatHandler, SeatState},
    seat::pointer::{PointerHandler, PointerEvent, PointerEventKind},
    shell::{
        wlr_layer::{
            Anchor, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

// ============================================================================
// Constants
// ============================================================================

/// Fixed widget width in pixels (height is dynamic based on content)
const WIDGET_WIDTH: u32 = 370;
/// Default/initial widget height (recalculated based on enabled sections)
const WIDGET_HEIGHT: u32 = 400;

// ============================================================================
// Main Widget State Structure
// ============================================================================

/// Main state structure for the monitoring widget.
///
/// Holds all Wayland protocol state, monitoring modules, and UI state.
/// This struct implements multiple Wayland handler traits to receive
/// compositor events.
struct MonitorWidget {
    // === Wayland Protocol State ===
    // These are required by smithay-client-toolkit for Wayland communication
    
    /// Global registry for discovering Wayland interfaces
    registry_state: RegistryState,
    /// Information about available outputs (monitors)
    output_state: OutputState,
    /// Wayland compositor interface for surface creation
    compositor_state: CompositorState,
    /// Shared memory interface for buffer allocation
    shm_state: Shm,
    /// Layer shell interface for desktop overlay surfaces
    layer_shell: LayerShell,
    /// Seat interface for input devices
    seat_state: SeatState,
    
    /// The layer surface we render to (created after initialization)
    layer_surface: Option<LayerSurface>,
    
    // === Configuration ===
    
    /// Current widget configuration (shared reference for thread safety)
    config: Arc<Config>,
    /// Handle to cosmic-config for saving position changes during drag
    config_handler: cosmic_config::Config,
    /// Last time we checked for config changes
    last_config_check: Instant,
    
    // === System Monitoring Modules ===
    // Each module is responsible for collecting and caching specific metrics
    
    /// CPU, Memory, and GPU utilization percentages
    utilization: UtilizationMonitor,
    /// CPU and GPU temperatures from sensors
    temperature: TemperatureMonitor,
    /// Network upload/download rates (currently unused in UI)
    network: NetworkMonitor,
    /// Weather data from OpenWeatherMap API
    weather: WeatherMonitor,
    /// Mounted disk space information
    storage: StorageMonitor,
    /// Battery levels from system and Solaar
    battery: BatteryMonitor,
    /// D-Bus desktop notifications
    notifications: NotificationMonitor,
    /// Now playing from Cider
    media: MediaMonitor,
    /// Last time system stats were updated
    last_update: Instant,
    
    // === Rendering State ===
    
    /// Shared memory pool for Wayland buffer allocation
    pool: Option<SlotPool>,
    /// Last rendered height (for detecting resize needs)
    last_height: u32,
    /// Last drawn clock second (for sync'd updates)
    last_drawn_second: Option<String>,
    
    // === Mouse Interaction State ===
    
    /// Whether user is currently dragging the widget
    dragging: bool,
    /// Starting X position of drag operation
    drag_start_x: f64,
    /// Starting Y position of drag operation
    drag_start_y: f64,
    
    // === Click Detection Bounds ===
    // These are populated by the renderer and used for hit testing
    
    /// Vertical bounds of the notification section (y_start, y_end)
    notification_bounds: Option<(f64, f64)>,
    /// Bounds of notification group headers for collapse toggle
    /// Format: [(app_name, y_start, y_end)]
    notification_group_bounds: Vec<(String, f64, f64)>,
    /// Bounds of X buttons for clearing groups/notifications
    /// Format: [(key, x_start, y_start, x_end, y_end)]
    /// Key is "app_name" for groups, "app_name:timestamp" for individual
    notification_clear_bounds: Vec<(String, f64, f64, f64, f64)>,
    /// Bounds of the "Clear All" button
    clear_all_bounds: Option<(f64, f64, f64, f64)>,
    /// Bounds of media playback control buttons
    /// Format: [(button_name, x_start, y_start, x_end, y_end)]
    media_button_bounds: Vec<(String, f64, f64, f64, f64)>,
    
    // === Notification UI State ===
    
    /// Set of app names whose notification groups are collapsed
    collapsed_groups: std::collections::HashSet<String>,
    /// Cached grouped notifications to avoid recomputing each frame
    grouped_notifications: Vec<(String, Vec<widget::notifications::Notification>)>,
    /// Version counter to detect notification changes
    notifications_version: u64,
    
    // === Control Flags ===
    
    /// Set to true when UI changes require immediate redraw
    force_redraw: bool,
    /// Last click timestamp for debouncing rapid clicks
    last_click_time: std::time::Instant,
    /// Set to true when compositor requests close
    exit: bool,
    
    // === Theme ===
    
    /// Current COSMIC theme (accent color, dark/light mode)
    theme: CosmicTheme,
    /// Last time we checked for theme changes
    last_theme_check: Instant,
}

// ============================================================================
// Wayland Handler Implementations
// ============================================================================
// These traits are required by smithay-client-toolkit to receive events from
// the Wayland compositor. Each handler processes specific event types.

/// Handles compositor events like scale factor changes and frame callbacks.
impl CompositorHandler for MonitorWidget {
    /// Called when the output scale factor changes (e.g., HiDPI).
    /// Currently ignored - could be used for HiDPI rendering support.
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
        // Handle scale factor changes if needed
    }

    /// Called when display transform changes (rotation).
    /// Currently ignored - could rotate the widget to match.
    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
        // Handle transform changes if needed
    }

    /// Frame callback - compositor is ready for next frame.
    /// This triggers a redraw with the current timestamp.
    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.draw(qh, chrono::Local::now(), true);
    }

    /// Called when surface enters an output (becomes visible).
    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    /// Called when surface leaves an output (no longer visible).
    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

/// Handles output (display) events.
/// Currently unused but required by the registry.
impl OutputHandler for MonitorWidget {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

/// Handles layer-shell specific events.
/// Layer-shell allows creating surfaces outside normal window management.
impl LayerShellHandler for MonitorWidget {
    /// Called when compositor closes our layer surface.
    /// Sets exit flag to terminate the main loop cleanly.
    fn closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
    ) {
        self.exit = true;
    }

    /// Called when compositor configures our layer surface.
    /// This happens after initial creation and when size changes are acknowledged.
    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if configure.new_size.0 == 0 || configure.new_size.1 == 0 {
            // Use our default size
        }
        self.draw(qh, chrono::Local::now(), true);
    }
}

/// Handles input seat events (keyboard/mouse capability changes).
impl SeatHandler for MonitorWidget {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wayland_client::protocol::wl_seat::WlSeat) {
        log::info!("New seat detected");
    }
    
    /// Called when a seat gains a new capability (pointer, keyboard, touch).
    /// We request pointer events when pointer capability is available.
    fn new_capability(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, seat: wayland_client::protocol::wl_seat::WlSeat, capability: Capability) {
        log::info!("New capability: {:?}", capability);
        if capability == Capability::Pointer {
            // Request pointer events
            log::info!("Requesting pointer events from seat");
            let _ = self.seat_state.get_pointer(qh, &seat);
        }
    }
    fn remove_capability(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wayland_client::protocol::wl_seat::WlSeat, _capability: Capability) {}
    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wayland_client::protocol::wl_seat::WlSeat) {}
}

/// Handles mouse pointer events.
/// This is where all click interactions are processed.
impl PointerHandler for MonitorWidget {
    /// Process batched pointer events.
    /// Events include clicks (Press/Release) and motion.
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wayland_client::protocol::wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            // Log all pointer events for debugging
            match &event.kind {
                PointerEventKind::Enter { .. } => {
                    log::info!("Pointer entered widget surface at ({}, {})", event.position.0, event.position.1);
                }
                PointerEventKind::Leave { .. } => {
                    log::info!("Pointer left widget surface");
                }
                PointerEventKind::Motion { .. } => {
                    // Don't log motion events, too noisy
                }
                _ => {}
            }
            
            match event.kind {
                // === Left-click handling (when NOT in drag mode) ===
                // Handles clicks on: Clear All, individual notification X buttons,
                // group collapse/expand, and media playback controls.
                PointerEventKind::Press { button, .. } if button == 0x110 => {
                    log::info!("Left-click detected at ({}, {}), widget_movable={}", event.position.0, event.position.1, self.config.widget_movable);
                    
                    // If widget is movable, don't process clicks (allow drag instead)
                    if self.config.widget_movable {
                        log::debug!("Widget is movable, skipping click handling");
                        continue;
                    }
                    
                    // Debounce: ignore clicks within 200ms of each other
                    let now = Instant::now();
                    if now.duration_since(self.last_click_time).as_millis() < 200 {
                        log::debug!("Ignoring rapid click (debounced)");
                        continue;
                    }
                    self.last_click_time = now;
                    
                    let click_x = event.position.0;
                    let click_y = event.position.1;
                    
                    log::debug!("Click at ({}, {})", click_x, click_y);
                    
                    let mut handled = false;
                    
                    // Priority 1: Check "Clear All" button (top of notification section)
                    if let Some((x_start, y_start, x_end, y_end)) = self.clear_all_bounds {
                        if click_x >= x_start && click_x <= x_end && click_y >= y_start && click_y <= y_end {
                            log::info!("Clear All button clicked at ({}, {})", click_x, click_y);
                            self.notifications.clear();
                            self.collapsed_groups.clear();
                            self.force_redraw = true;
                            handled = true;
                        }
                    }
                    
                    // Priority 2: Check notification X buttons (group clear or individual dismiss)
                    // Key format: "app_name" for groups, "app_name:timestamp" for individual
                    if !handled {
                        for (key, x_start, y_start, x_end, y_end) in &self.notification_clear_bounds {
                            log::trace!("Checking X button for {}: ({}-{}, {}-{})", key, x_start, x_end, y_start, y_end);
                            if click_x >= *x_start && click_x <= *x_end && click_y >= *y_start && click_y <= *y_end {
                                // Check if this is an individual notification dismiss (format: "app_name:timestamp")
                                // or a group clear (format: just "app_name")
                                if let Some((app_name, timestamp_str)) = key.split_once(':') {
                                    // Individual notification dismiss
                                    if let Ok(timestamp) = timestamp_str.parse::<u64>() {
                                        log::info!("Dismissing notification: {} at timestamp {} (click at {}, {})", app_name, timestamp, click_x, click_y);
                                        self.notifications.remove_notification(app_name, timestamp);
                                        self.force_redraw = true;
                                        handled = true;
                                        break;
                                    }
                                } else {
                                    // Group clear
                                    log::info!("Clearing notification group: {} at ({}, {})", key, click_x, click_y);
                                    self.notifications.clear_app(key);
                                    self.collapsed_groups.remove(key);
                                    self.force_redraw = true;
                                    handled = true;
                                    break;
                                }
                            }
                        }
                    }
                    
                    // Priority 3: Check notification group headers for collapse/expand toggle
                    // Clicking a group header (excluding X button area) toggles visibility
                    if !handled {
                        for (app_name, y_start, y_end) in &self.notification_group_bounds {
                            log::trace!("Checking group header for {}: {}-{}", app_name, y_start, y_end);
                            if click_y >= *y_start && click_y <= *y_end {
                                // Make sure we're not clicking the X button area
                                // X button is at x=340, with radius 7, so roughly 333-347
                                if click_x < 333.0 {
                                    log::debug!("Toggling notification group: {}", app_name);
                                    if self.collapsed_groups.contains(app_name) {
                                        self.collapsed_groups.remove(app_name);
                                    } else {
                                        self.collapsed_groups.insert(app_name.clone());
                                    }
                                    self.force_redraw = true;
                                    handled = true;
                                    break;
                                } else {
                                    log::debug!("Click in X button area (x={:.1}), not toggling", click_x);
                                }
                            }
                        }
                    }
                    
                    // Priority 4: Check media control buttons (previous, play/pause, next, progress_bar, player_dot_N)
                    if !handled {
                        for (button_name, x_start, y_start, x_end, y_end) in &self.media_button_bounds {
                            if click_x >= *x_start && click_x <= *x_end && click_y >= *y_start && click_y <= *y_end {
                                log::info!("Media button '{}' clicked at ({}, {})", button_name, click_x, click_y);
                                // Debug: log current player state
                                let player_state = self.media.get_player_state();
                                log::info!("Player state: {} players, current_index={}", player_state.player_count(), player_state.current_index);
                                if let Some((id, info)) = player_state.current_player() {
                                    log::info!("Current player: {:?}, title: {}", id, info.title);
                                } else {
                                    log::warn!("No current player available!");
                                }
                                match button_name.as_str() {
                                    "play_pause" => {
                                        self.media.play_pause();
                                    }
                                    "next" => {
                                        self.media.next();
                                    }
                                    "previous" => {
                                        self.media.previous();
                                    }
                                    "progress_bar" => {
                                        // Calculate progress based on click position within the bar
                                        let bar_width = x_end - x_start;
                                        let click_offset = click_x - x_start;
                                        let progress = (click_offset / bar_width).clamp(0.0, 1.0);
                                        log::info!("Progress bar clicked: {:.1}%", progress * 100.0);
                                        self.media.seek_to_progress(progress);
                                    }
                                    name if name.starts_with("player_dot_") => {
                                        // Extract player index from button name
                                        if let Some(index_str) = name.strip_prefix("player_dot_") {
                                            if let Ok(index) = index_str.parse::<usize>() {
                                                log::info!("Switching to player {}", index);
                                                self.media.select_player(index);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                                self.force_redraw = true;
                                handled = true;
                                break;
                            }
                        }
                    }
                    
                    if handled {
                        log::debug!("Notification action handled, forcing redraw");
                    } else {
                        log::debug!("Click at ({:.1}, {:.1}) not handled by any notification element", click_x, click_y);
                    }
                }
                
                // === Right-click: Quick clear notifications in section ===
                PointerEventKind::Press { button, .. } if button == 0x111 => {
                    if let Some((y_start, y_end)) = self.notification_bounds {
                        let click_y = event.position.1;
                        if click_y >= y_start && click_y <= y_end {
                            log::info!("Right-click on notifications section, clearing");
                            self.notifications.clear();
                            self.collapsed_groups.clear();
                            // Set flag to force redraw on next frame
                            self.force_redraw = true;
                        }
                    }
                }
                
                // === Widget Dragging (only when movable mode is enabled) ===
                // This is activated when the settings window is open
                
                // Start drag on left-click
                PointerEventKind::Press { button, .. } if button == 0x110 && self.config.widget_movable => {
                    self.dragging = true;
                    self.drag_start_x = event.position.0;
                    self.drag_start_y = event.position.1;
                }
                
                // End drag on release
                PointerEventKind::Release { button, .. } if button == 0x110 && self.config.widget_movable => {
                    self.dragging = false;
                }
                
                // Update position while dragging (saves to config for persistence)
                PointerEventKind::Motion { .. } if self.dragging && self.config.widget_movable => {
                    let delta_x = (event.position.0 - self.drag_start_x) as i32;
                    let delta_y = (event.position.1 - self.drag_start_y) as i32;
                    
                    let mut new_config = (*self.config).clone();
                    new_config.widget_x += delta_x;
                    new_config.widget_y += delta_y;
                    
                    if new_config.write_entry(&self.config_handler).is_ok() {
                        self.config = Arc::new(new_config);
                        
                        if let Some(layer_surface) = &self.layer_surface {
                            layer_surface.set_margin(self.config.widget_y, 0, 0, self.config.widget_x);
                            layer_surface.commit();
                        }
                    }
                    
                    self.drag_start_x = event.position.0;
                    self.drag_start_y = event.position.1;
                }
                _ => {}
            }
        }
    }
}

/// Handles shared memory buffer allocation for Wayland rendering.
impl ShmHandler for MonitorWidget {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

// ============================================================================
// MonitorWidget Implementation
// ============================================================================

impl MonitorWidget {
    /// Create a new MonitorWidget with all necessary Wayland state.
    ///
    /// # Arguments
    /// * `globals` - Wayland global registry
    /// * `qh` - Queue handle for event dispatching
    /// * `config` - Initial configuration
    /// * `config_handler` - Handle for saving config changes
    fn new(
        globals: &wayland_client::globals::GlobalList,
        qh: &QueueHandle<Self>,
        config: Config,
        config_handler: cosmic_config::Config,
    ) -> Self {
        let registry_state = RegistryState::new(globals);
        let output_state = OutputState::new(globals, qh);
        let compositor_state = CompositorState::bind(globals, qh)
            .expect("wl_compositor not available");
        let shm_state = Shm::bind(globals, qh).expect("wl_shm not available");
        let layer_shell = LayerShell::bind(globals, qh).expect("layer shell not available");
        let seat_state = SeatState::new(globals, qh);

        // Clone weather config values before moving config
        let weather_api_key = config.weather_api_key.clone();
        let weather_location = config.weather_location.clone();
        let cider_api_token = if config.cider_api_token.is_empty() {
            None
        } else {
            Some(config.cider_api_token.clone())
        };

        Self {
            registry_state,
            output_state,
            compositor_state,
            shm_state,
            layer_shell,
            seat_state,
            layer_surface: None,
            config: Arc::new(config),
            config_handler,
            last_config_check: Instant::now(),
            utilization: UtilizationMonitor::new(),
            temperature: TemperatureMonitor::new(),
            network: NetworkMonitor::new(),
            weather: WeatherMonitor::new(weather_api_key, weather_location),
            storage: StorageMonitor::new(),
            battery: BatteryMonitor::new(),
            notifications: NotificationMonitor::new(5), // Keep last 5 notifications
            media: MediaMonitor::new(cider_api_token),
            last_update: Instant::now(),
            pool: None,
            last_height: WIDGET_HEIGHT,
            last_drawn_second: None,
            dragging: false,
            drag_start_x: 0.0,
            drag_start_y: 0.0,
            notification_bounds: None,
            notification_group_bounds: Vec::new(),
            notification_clear_bounds: Vec::new(),
            clear_all_bounds: None,
            media_button_bounds: Vec::new(),
            collapsed_groups: std::collections::HashSet::new(),
            grouped_notifications: Vec::new(),
            notifications_version: 0,
            force_redraw: false,
            last_click_time: Instant::now(),
            exit: false,
            theme: CosmicTheme::load(),
            last_theme_check: Instant::now(),
        }
    }

    /// Create the layer surface for desktop overlay rendering.
    ///
    /// Configures the surface to:
    /// - Anchor to top-left corner with offset from config
    /// - Use Layer::Bottom so windows can cover the widget
    /// - Not reserve exclusive space
    /// - Accept keyboard input on demand (for future features)
    fn create_layer_surface(&mut self, qh: &QueueHandle<Self>) {
        let surface = self.compositor_state.create_surface(qh);
        
        let layer_surface = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Bottom,  // Below windows, acts like desktop widget
            Some("cosmic-widget"),
            None,
        );

        // Configure the layer surface
        layer_surface.set_anchor(Anchor::TOP | Anchor::LEFT); // Anchor to top-left corner
        layer_surface.set_size(WIDGET_WIDTH, WIDGET_HEIGHT);
        layer_surface.set_exclusive_zone(-1); // Don't reserve space
        log::debug!("Setting layer surface margins: top={}, left={}", self.config.widget_y, self.config.widget_x);
        layer_surface.set_margin(self.config.widget_y, 0, 0, self.config.widget_x);
        // Use OnDemand to get input focus when clicked - improves input responsiveness
        layer_surface.set_keyboard_interactivity(
            smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity::OnDemand
        );
        
        layer_surface.commit();
        
        self.layer_surface = Some(layer_surface);
    }

    /// Update system statistics from all enabled monitoring modules.
    ///
    /// Respects the configured update interval to avoid excessive polling.
    /// Only updates modules that are currently enabled in the config.
    fn update_system_stats(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        
        if elapsed < (self.config.update_interval_ms as f64 / 1000.0) {
            return;
        }
        
        self.last_update = now;

        log::trace!("Updating system stats");

        // Update monitoring modules (only if enabled)
        if self.config.show_cpu || self.config.show_memory || self.config.show_gpu {
            log::trace!("Updating CPU/Memory/GPU utilization");
            self.utilization.update();
        }
        
        if self.config.show_cpu_temp || self.config.show_gpu_temp {
            log::trace!("Updating temperature");
            self.temperature.update();
        }
        
        if self.config.show_network {
            log::trace!("Updating network");
            self.network.update();
        }
        
        // Update storage
        if self.config.show_storage {
            log::trace!("Updating storage");
            self.storage.update();
            log::trace!("Storage updated, {} disks found", self.storage.disk_info.len());
        }

        // Update battery info only when the section and Solaar integration are enabled
        if self.config.show_battery && self.config.enable_solaar_integration {
            log::trace!("Updating battery info from Solaar");
            self.battery.update();
        }
        
        // Update weather (has its own rate limiting - every 10 minutes)
        if self.config.show_weather {
            log::trace!("Requesting weather update");
            self.weather.update();
        }
        
        // Update grouped notifications cache if notifications changed
        if self.config.show_notifications {
            self.update_notification_groups();
        }
        
        log::trace!("System stats update complete");
    }
    
    /// Update the cached notification groups.
    ///
    /// Groups notifications by app name and sorts by most recent.
    /// Only recomputes if the notification count has changed.
    fn update_notification_groups(&mut self) {
        let notifications = self.notifications.get_notifications();
        let new_version = notifications.len() as u64;
        
        // Only recompute if notifications changed
        if new_version != self.notifications_version {
            use std::collections::HashMap;
            
            // Group notifications by app name
            let mut grouped: HashMap<String, Vec<widget::notifications::Notification>> = HashMap::new();
            for n in notifications {
                grouped.entry(n.app_name.clone())
                       .or_default()
                       .push(n);
            }
            
            // Convert to vec and sort by most recent notification
            let mut groups: Vec<_> = grouped.into_iter().collect();
            groups.sort_by(|a, b| {
                let a_latest = a.1.iter().map(|n| n.timestamp).max().unwrap_or(0);
                let b_latest = b.1.iter().map(|n| n.timestamp).max().unwrap_or(0);
                b_latest.cmp(&a_latest)
            });
            
            self.grouped_notifications = groups;
            self.notifications_version = new_version;
            log::trace!("Notification groups updated: {} groups", self.grouped_notifications.len());
        }
    }

    /// Render the widget to the Wayland surface.
    ///
    /// This is the main rendering function that:
    /// 1. Calculates dynamic height based on enabled sections
    /// 2. Allocates/resizes the shared memory buffer
    /// 3. Calls the Cairo renderer to draw all sections
    /// 4. Updates click bounds for interactive elements
    /// 5. Commits the buffer to the compositor
    ///
    /// # Arguments
    /// * `qh` - Queue handle (unused but required by trait)
    /// * `current_time` - Time to display on clock
    /// * `update_stats` - Whether to poll system statistics
    fn draw(&mut self, _qh: &QueueHandle<Self>, current_time: chrono::DateTime<chrono::Local>, update_stats: bool) {
        let layer_surface = match &self.layer_surface {
            Some(ls) => ls.clone(),
            None => {
                log::warn!("No layer surface available for drawing");
                return;
            }
        };

        // Only update system stats for timed updates, not for UI-only redraws
        if update_stats {
            self.update_system_stats();
        }
        
        // Calculate dynamic height based on enabled components
        let disk_count = if self.config.show_storage { self.storage.disk_info.len() } else { 0 };
        let battery_count = if self.config.show_battery { self.battery.devices().len() } else { 0 };
        let notification_count = if self.config.show_notifications { self.notifications.get_notifications().len() } else { 0 };
        let player_count = if self.config.show_media { self.media.get_player_state().player_count() } else { 0 };
        let width = WIDGET_WIDTH as i32;
        let height = calculate_widget_height_with_all(&self.config, disk_count, battery_count, notification_count, player_count) as i32;
        let stride = width * 4;

        log::trace!("Drawing widget: {}x{} (disks: {})", width, height, disk_count);

        // Update layer surface size if height changed OR create pool if it doesn't exist
        if height as u32 != self.last_height || self.pool.is_none() {
            log::debug!("Updating surface size to {}x{}", width, height);
            self.last_height = height as u32;
            layer_surface.set_size(width as u32, height as u32);
            layer_surface.commit();
            
            // Recreate pool with new size
            self.pool = Some(SlotPool::new(width as usize * height as usize * 4, &self.shm_state)
                .expect("Failed to create pool"));
        }

        // Store the data we need for rendering
        let cpu_usage = self.utilization.cpu_usage;
        let memory_usage = self.utilization.memory_usage;
        let gpu_usage = self.utilization.get_gpu_usage();
        let cpu_temp = self.temperature.cpu_temp;
        let gpu_temp = self.temperature.gpu_temp;
        let network_rx_rate = self.network.network_rx_rate;
        let network_tx_rate = self.network.network_tx_rate;
        let show_cpu = self.config.show_cpu;
        let show_memory = self.config.show_memory;
        let show_network = self.config.show_network;
        let show_disk = self.config.show_disk;
        let show_storage = self.config.show_storage;
        let show_gpu = self.config.show_gpu;
        let show_cpu_temp = self.config.show_cpu_temp;
        let show_gpu_temp = self.config.show_gpu_temp;
        let show_clock = self.config.show_clock;
        let show_date = self.config.show_date;
        let show_percentages = self.config.show_percentages;
        let use_24hour_time = self.config.use_24hour_time;
        let use_circular_temp_display = self.config.use_circular_temp_display;
        let show_weather = self.config.show_weather;
        let show_battery = self.config.show_battery;
        let enable_solaar_integration = self.config.enable_solaar_integration;
        
        // Extract weather data
        let (weather_temp, weather_desc, weather_location, weather_icon) = {
            let weather_data_guard = self.weather.weather_data.lock().unwrap();
            if let Some(ref data) = *weather_data_guard {
                (data.temperature, data.description.clone(), data.location.clone(), data.icon.clone())
            } else {
                (f32::NAN, String::from("No data"), String::from("Unknown"), String::from("01d"))
            }
        };
        
        let weather_desc = weather_desc.as_str();
        let weather_location = weather_location.as_str();
        let weather_icon = weather_icon.as_str();

        // Snapshot battery devices for this frame
        let battery_devices = self.battery.devices();
        
        // Use cached grouped notifications (updated in update_system_stats)
        let grouped_notifications = &self.grouped_notifications;

        let pool = self.pool.as_mut().unwrap();

        let (buffer, canvas) = pool
            .create_buffer(width, height, stride, wl_shm::Format::Argb8888)
            .expect("Failed to create buffer");

        // Get media info
        let player_state = self.media.get_player_state();
        let media_info = player_state.current_player()
            .map(|(_, info)| info.clone())
            .unwrap_or_default();
        let player_count = player_state.player_count();
        let current_player_index = player_state.current_index;
        
        // Use Cairo for rendering
        let params = RenderParams {
            width,
            height,
            cpu_usage,
            memory_usage,
            gpu_usage,
            cpu_temp,
            gpu_temp,
            network_rx_rate,
            network_tx_rate,
            show_cpu,
            show_memory,
            show_network,
            show_disk,
            show_storage,
            show_gpu,
            show_cpu_temp,
            show_gpu_temp,
            show_clock,
            show_date,
            show_percentages,
            use_24hour_time,
            use_circular_temp_display,
            show_weather,
            show_battery,
            show_notifications: self.config.show_notifications,
            show_media: self.config.show_media,
            enable_solaar_integration,
            weather_temp,
            weather_desc,
            weather_location,
            weather_icon,
            disk_info: &self.storage.disk_info,
            battery_devices: &battery_devices,
            grouped_notifications,
            collapsed_groups: &self.collapsed_groups,
            media_info: &media_info,
            player_count,
            current_player_index,
            section_order: &self.config.section_order,
            current_time,
            theme: &self.theme,
        };
        
        // Wrap rendering in panic catch to prevent crashes
        let render_start = Instant::now();
        let render_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render_widget(canvas, params)
        }));
        log::info!("Cairo render took: {:?}", render_start.elapsed());
        
        match render_result {
            Ok((bounds, groups, clear_bounds, clear_all, media_bounds)) => {
                let group_count = groups.len();
                self.notification_bounds = bounds;
                self.notification_group_bounds = groups;
                self.notification_clear_bounds = clear_bounds;
                self.clear_all_bounds = clear_all;
                self.media_button_bounds = media_bounds;
                log::trace!("Render successful, {} notification groups", group_count);
            }
            Err(e) => {
                log::error!("Panic occurred during rendering: {:?}", e);
                // Clear potentially corrupted state
                self.notification_group_bounds.clear();
                self.notification_clear_bounds.clear();
                self.clear_all_bounds = None;
                self.media_button_bounds.clear();
                return; // Skip this frame
            }
        }

        // Attach the buffer to the surface
        layer_surface
            .wl_surface()
            .attach(Some(buffer.wl_buffer()), 0, 0);
        layer_surface.wl_surface().damage_buffer(0, 0, width, height);
        
        // Commit changes
        layer_surface.wl_surface().commit();
    }
}

// Empty impl block - keeping for potential future private helpers
impl MonitorWidget {
}

// ============================================================================
// smithay-client-toolkit Delegation Macros
// ============================================================================
// These macros generate the boilerplate code to route Wayland events
// to our handler implementations above.

delegate_compositor!(MonitorWidget);
delegate_output!(MonitorWidget);
delegate_shm!(MonitorWidget);
delegate_seat!(MonitorWidget);
delegate_pointer!(MonitorWidget);
delegate_layer!(MonitorWidget);

delegate_registry!(MonitorWidget);

/// Provides access to the registry state for other handlers.
impl ProvidesRegistryState for MonitorWidget {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

// ============================================================================
// Main Entry Point
// ============================================================================

/// Widget main function with Wayland reconnection support.
///
/// The main loop:
/// 1. Connects to Wayland compositor
/// 2. Creates the layer surface
/// 3. Enters event loop (dispatch, draw, flush)
/// 4. On connection error, attempts reconnection with backoff
///
/// # Error Handling
///
/// Non-recoverable errors (e.g., layer-shell not available) cause immediate exit.
/// Recoverable errors (broken pipe) trigger reconnection.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Ignore SIGPIPE so a closed socket becomes a normal EPIPE result, not a signal.
    // This prevents the process from being killed when the compositor closes the connection.
    unsafe { 
        libc::signal(libc::SIGPIPE, libc::SIG_IGN); 
    }
    
    // Load configuration to check if logging should be enabled
    let config_handler = cosmic_config::Config::new(
        "com.github.zoliviragh.CosmicWidget",
        Config::VERSION,
    )?;
    
    let mut base_config = Config::get_entry(&config_handler).unwrap_or_default();
    
    // Initialize logger only if enabled in config
    if base_config.enable_logging {
        use std::fs::OpenOptions;
        
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/cosmic-widget.log")
            .expect("Failed to open log file");
        
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .target(env_logger::Target::Pipe(Box::new(log_file)))
            .init();
        
        log::info!("Starting COSMIC Monitor Widget (logging enabled)");
        log::info!("Widget starting with position: X={}, Y={}", base_config.widget_x, base_config.widget_y);
        log::info!("Weather enabled: {}, API key set: {}", base_config.show_weather, !base_config.weather_api_key.is_empty());
        log::info!("Notifications enabled: {}, section_order: {:?}", base_config.show_notifications, base_config.section_order);
    }
    
    // Load custom Weather Icons font for weather display
    load_weather_font();

    // === Reconnection Loop ===
    // Uses exponential backoff: 1s, 2s, 5s, 10s, 20s, 30s, then cycles
    let mut backoff_secs = [1_u64, 2, 5, 10, 20, 30].into_iter().cycle();

    'reconnect: loop {
        log::info!("Connecting to Wayland...");
        
        // Connect to Wayland
        let conn = Connection::connect_to_env()?;
        let (globals, mut event_queue) = registry_queue_init(&conn)?;
        let qh = event_queue.handle();

        log::info!("Connected to Wayland server");

        // Create widget for this connection
        let mut widget = MonitorWidget::new(&globals, &qh, base_config.clone(), config_handler.clone());
        widget.create_layer_surface(&qh);
        
        // Perform initial roundtrip to receive configure event from compositor
        log::info!("Waiting for compositor configure event...");
        if let Err(e) = event_queue.roundtrip(&mut widget) {
            log::warn!("Roundtrip failed: {}. Reconnecting...", e);
            let d = Duration::from_secs(backoff_secs.next().unwrap());
            thread::sleep(d);
            continue 'reconnect;
        }

        log::info!("Widget initialized, entering main loop");

        let mut last_heartbeat = Instant::now();

        // === Session Event Loop ===
        // Processes events until connection is lost or exit is requested
        'session: loop {
            let now = Instant::now();
            
            // === Event Dispatch ===
            // Use roundtrip to ensure all pending events are processed
            log::trace!("Roundtrip to get events");
            if let Err(e) = event_queue.roundtrip(&mut widget) {
                log::error!("Error in roundtrip: {}", e);
                
                // Check for broken pipe in error message - reconnect if so
                let error_str = e.to_string();
                if error_str.contains("Broken pipe") || error_str.contains("os error 32") {
                    log::warn!("Broken pipe during roundtrip → reconnecting");
                    break 'session;
                }
                
                return Err(e.into());
            }
            log::trace!("Roundtrip complete");
            
            // === Clock Synchronization ===
            // Display time offset by 1 second to match typical system clock behavior
            let current_time = chrono::Local::now();
            let display_time = current_time - chrono::Duration::seconds(1);
            let current_second = display_time.format("%S").to_string();
            
            // === Immediate UI Redraw ===
            // Fast path for notification/media interactions (skip system stats update)
            if widget.force_redraw {
                widget.draw(&qh, display_time, false);
                widget.force_redraw = false;
                // Immediately flush to ensure compositor receives the update
                let _ = conn.flush();
            }
            
            // === Second-Based Redraw ===
            // Full redraw with system stats when clock second changes
            let should_redraw = if let Some(ref last_sec) = widget.last_drawn_second {
                &current_second != last_sec
            } else {
                true // First draw
            };
            
            // Periodic full update with system stats
            if should_redraw {
                widget.draw(&qh, display_time, true);
                widget.last_drawn_second = Some(current_second);
            }
            
            // === Config Hot-Reload ===
            // Check for external config changes every 500ms (from settings app)
            if now.duration_since(widget.last_config_check).as_millis() > 500 {
                widget.last_config_check = now;
                if let Ok(new_config) = Config::get_entry(&widget.config_handler) {
                    // Only update if config actually changed
                    if *widget.config != new_config {
                        log::info!("Configuration changed, updating widget");
                        
                        // Keep latest config for future sessions
                        base_config = new_config.clone();
                        
                        // Update weather monitor if API key or location changed
                        if widget.config.weather_api_key != new_config.weather_api_key {
                            log::info!("Weather API key changed");
                            widget.weather.set_api_key(new_config.weather_api_key.clone());
                        }
                        if widget.config.weather_location != new_config.weather_location {
                            log::info!("Weather location changed to: {}", new_config.weather_location);
                            widget.weather.set_location(new_config.weather_location.clone());
                        }
                        
                        widget.config = Arc::new(new_config);
                        // Force a redraw with full stats update
                        widget.draw(&qh, chrono::Local::now(), true);
                    }
                }
            }
            
            // === Theme Hot-Reload ===
            // Check for theme changes every 2 seconds (less frequent than config)
            if now.duration_since(widget.last_theme_check).as_secs() >= 2 {
                widget.last_theme_check = now;
                let new_theme = CosmicTheme::load();
                // Check if accent color or dark mode changed
                if (new_theme.accent.red - widget.theme.accent.red).abs() > 0.01
                    || (new_theme.accent.green - widget.theme.accent.green).abs() > 0.01
                    || (new_theme.accent.blue - widget.theme.accent.blue).abs() > 0.01
                    || new_theme.is_dark != widget.theme.is_dark
                {
                    log::info!("Theme changed, reloading");
                    widget.theme = new_theme;
                    widget.draw(&qh, chrono::Local::now(), true);
                }
            }

            // === Heartbeat Logging ===
            // Log every 5 seconds to confirm widget is still running
            if now.duration_since(last_heartbeat) >= Duration::from_secs(5) {
                log::info!("Heartbeat: widget still running");
                last_heartbeat = now;
            }
            
            // === Connection Flush ===
            // Must flush frequently to keep connection alive (Wayland best practice)
            log::trace!("Flushing connection");
            if let Err(e) = conn.flush() {
                log::error!("Error flushing connection: {}", e);
                
                // Check for broken pipe in error message - reconnect if so
                let error_str = e.to_string();
                if error_str.contains("Broken pipe") || error_str.contains("os error 32") {
                    log::warn!("Broken pipe on flush → reconnecting");
                    break 'session;
                }
                
                return Err(e.into());
            }
            log::trace!("Flush complete");
            
            // === Frame Pacing ===
            // Small sleep to avoid busy-waiting while staying responsive (~60 FPS)
            thread::sleep(Duration::from_millis(16));

            // === Exit Check ===
            if widget.exit {
                log::info!("Exit requested, shutting down");
                return Ok(());
            }
        } // end 'session

        // === Reconnection Backoff ===
        // Wait before attempting to reconnect to avoid spinning
        let d = Duration::from_secs(backoff_secs.next().unwrap());
        log::info!("Reconnecting in {:?}...", d);
        thread::sleep(d);
        // Loop continues to 'reconnect...
    }
}
