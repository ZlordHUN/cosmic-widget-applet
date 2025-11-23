// SPDX-License-Identifier: MPL-2.0

//! Widget implementation using Wayland layer-shell protocol
//! This bypasses the compositor's window management to achieve borderless rendering

mod config;
mod widget;

use config::Config;
use widget::{UtilizationMonitor, TemperatureMonitor, NetworkMonitor, WeatherMonitor, StorageMonitor, BatteryMonitor, NotificationMonitor};
use widget::renderer::{render_widget, RenderParams};
use widget::layout::calculate_widget_height_with_all;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::thread;

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

const WIDGET_WIDTH: u32 = 370;
const WIDGET_HEIGHT: u32 = 400;

struct MonitorWidget {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor_state: CompositorState,
    shm_state: Shm,
    layer_shell: LayerShell,
    seat_state: SeatState,
    
    /// The main surface for rendering
    layer_surface: Option<LayerSurface>,
    
    /// Configuration
    config: Arc<Config>,
    config_handler: cosmic_config::Config,
    last_config_check: Instant,
    
    /// System monitoring modules
    utilization: UtilizationMonitor,
    temperature: TemperatureMonitor,
    network: NetworkMonitor,
    weather: WeatherMonitor,
    storage: StorageMonitor,
    battery: BatteryMonitor,
    notifications: NotificationMonitor,
    last_update: Instant,
    
    /// Memory pool for rendering
    pool: Option<SlotPool>,
    
    /// Track last widget height for resizing
    last_height: u32,
    
    /// Track last drawn second to synchronize clock updates
    last_drawn_second: Option<String>,
    
    /// Mouse dragging state
    dragging: bool,
    drag_start_x: f64,
    drag_start_y: f64,
    
    /// Notification section bounds (y_start, y_end)
    notification_bounds: Option<(f64, f64)>,
    
    /// Group bounds for notifications [(app_name, y_start, y_end)]
    notification_group_bounds: Vec<(String, f64, f64)>,
    
    /// Clear button bounds for each group [(app_name, x_start, y_start, x_end, y_end)]
    notification_clear_bounds: Vec<(String, f64, f64, f64, f64)>,
    
    /// Clear all button bounds (x_start, y_start, x_end, y_end)
    clear_all_bounds: Option<(f64, f64, f64, f64)>,
    
    /// Collapsed notification groups (app names)
    collapsed_groups: std::collections::HashSet<String>,
    
    /// Force redraw flag (set when notifications are cleared)
    force_redraw: bool,
    
    /// Last click timestamp to debounce rapid clicks
    last_click_time: std::time::Instant,
    
    /// Exit flag
    exit: bool,
}

impl CompositorHandler for MonitorWidget {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
        // Handle scale factor changes if needed
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
        // Handle transform changes if needed
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.draw(qh, chrono::Local::now());
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

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

impl LayerShellHandler for MonitorWidget {
    fn closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
    ) {
        self.exit = true;
    }

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
        self.draw(qh, chrono::Local::now());
    }
}

impl SeatHandler for MonitorWidget {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wayland_client::protocol::wl_seat::WlSeat) {}
    fn new_capability(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, seat: wayland_client::protocol::wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            // Request pointer events
            let _ = self.seat_state.get_pointer(qh, &seat);
        }
    }
    fn remove_capability(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wayland_client::protocol::wl_seat::WlSeat, _capability: Capability) {}
    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wayland_client::protocol::wl_seat::WlSeat) {}
}

impl PointerHandler for MonitorWidget {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wayland_client::protocol::wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            match event.kind {
                // Left-click (button 0x110) to toggle notification groups or clear
                PointerEventKind::Press { button, .. } if button == 0x110 && !self.config.widget_movable => {
                    // Debounce clicks - ignore if less than 200ms since last click
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
                    
                    // Check if clicking "Clear All" button
                    if let Some((x_start, y_start, x_end, y_end)) = self.clear_all_bounds {
                        if click_x >= x_start && click_x <= x_end && click_y >= y_start && click_y <= y_end {
                            log::info!("Clear All button clicked at ({}, {})", click_x, click_y);
                            self.notifications.clear();
                            self.collapsed_groups.clear();
                            self.force_redraw = true;
                            handled = true;
                        }
                    }
                    
                    // Check if clicking a group's clear button
                    if !handled {
                        for (app_name, x_start, y_start, x_end, y_end) in &self.notification_clear_bounds {
                            log::trace!("Checking X button for {}: ({}-{}, {}-{})", app_name, x_start, x_end, y_start, y_end);
                            if click_x >= *x_start && click_x <= *x_end && click_y >= *y_start && click_y <= *y_end {
                                log::info!("Clearing notification group: {} at ({}, {})", app_name, click_x, click_y);
                                self.notifications.clear_app(app_name);
                                self.collapsed_groups.remove(app_name);
                                self.force_redraw = true;
                                handled = true;
                                break;
                            }
                        }
                    }
                    
                    // Check if clicking a notification group header (to toggle)
                    if !handled {
                        for (app_name, y_start, y_end) in &self.notification_group_bounds {
                            log::trace!("Checking group header for {}: {}-{}", app_name, y_start, y_end);
                            if click_y >= *y_start && click_y <= *y_end {
                                // Make sure we're not clicking the X button area
                                // X button is at x=340, with radius 7, so roughly 333-347
                                if click_x < 333.0 {
                                    log::info!("Toggling notification group: {} at ({}, {})", app_name, click_x, click_y);
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
                    
                    if handled {
                        log::debug!("Notification action handled, forcing redraw");
                    } else {
                        log::debug!("Click at ({:.1}, {:.1}) not handled by any notification element", click_x, click_y);
                    }
                }
                // Right-click (button 0x111) to clear notifications
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
                // Widget movement (only if enabled)
                PointerEventKind::Press { button, .. } if button == 0x110 && self.config.widget_movable => {
                    self.dragging = true;
                    self.drag_start_x = event.position.0;
                    self.drag_start_y = event.position.1;
                }
                PointerEventKind::Release { button, .. } if button == 0x110 && self.config.widget_movable => {
                    self.dragging = false;
                }
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

impl ShmHandler for MonitorWidget {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

impl MonitorWidget {
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
            collapsed_groups: std::collections::HashSet::new(),
            force_redraw: false,
            last_click_time: Instant::now(),
            exit: false,
        }
    }

    fn create_layer_surface(&mut self, qh: &QueueHandle<Self>) {
        let surface = self.compositor_state.create_surface(qh);
        
        let layer_surface = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Top,  // Use Top layer for better interaction
            Some("cosmic-monitor-widget"),
            None,
        );

        // Configure the layer surface
        layer_surface.set_anchor(Anchor::TOP | Anchor::LEFT); // Anchor to top-left corner
        layer_surface.set_size(WIDGET_WIDTH, WIDGET_HEIGHT);
        layer_surface.set_exclusive_zone(-1); // Don't reserve space
        log::debug!("Setting layer surface margins: top={}, left={}", self.config.widget_y, self.config.widget_x);
        layer_surface.set_margin(self.config.widget_y, 0, 0, self.config.widget_x);
        layer_surface.set_keyboard_interactivity(
            smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity::None
        );
        
        layer_surface.commit();
        
        self.layer_surface = Some(layer_surface);
    }

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
        
        log::trace!("System stats update complete");
    }

    fn draw(&mut self, _qh: &QueueHandle<Self>, current_time: chrono::DateTime<chrono::Local>) {
        let layer_surface = match &self.layer_surface {
            Some(ls) => ls.clone(),
            None => {
                log::warn!("No layer surface available for drawing");
                return;
            }
        };

        self.update_system_stats();
        
        // Calculate dynamic height based on enabled components
        let disk_count = if self.config.show_storage { self.storage.disk_info.len() } else { 0 };
        let battery_count = if self.config.show_battery { self.battery.devices().len() } else { 0 };
        let notification_count = if self.config.show_notifications { self.notifications.get_notifications().len() } else { 0 };
        let width = WIDGET_WIDTH as i32;
        let height = calculate_widget_height_with_all(&self.config, disk_count, battery_count, notification_count) as i32;
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
        
        // Get notifications
        let notifications = self.notifications.get_notifications();

        let pool = self.pool.as_mut().unwrap();

        let (buffer, canvas) = pool
            .create_buffer(width, height, stride, wl_shm::Format::Argb8888)
            .expect("Failed to create buffer");

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
            enable_solaar_integration,
            weather_temp,
            weather_desc,
            weather_location,
            weather_icon,
            disk_info: &self.storage.disk_info,
            battery_devices: &battery_devices,
            notifications: &notifications,
            collapsed_groups: &self.collapsed_groups,
            section_order: &self.config.section_order,
            current_time,
        };
        
        // Wrap rendering in panic catch to prevent crashes
        let render_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render_widget(canvas, params)
        }));
        
        match render_result {
            Ok((bounds, groups, clear_bounds, clear_all)) => {
                let group_count = groups.len();
                self.notification_bounds = bounds;
                self.notification_group_bounds = groups;
                self.notification_clear_bounds = clear_bounds;
                self.clear_all_bounds = clear_all;
                log::trace!("Render successful, {} notification groups", group_count);
            }
            Err(e) => {
                log::error!("Panic occurred during rendering: {:?}", e);
                // Clear potentially corrupted state
                self.notification_group_bounds.clear();
                self.notification_clear_bounds.clear();
                self.clear_all_bounds = None;
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
        
        log::trace!("Frame rendered and committed successfully");
    }
}

impl MonitorWidget {
}

delegate_compositor!(MonitorWidget);
delegate_output!(MonitorWidget);
delegate_shm!(MonitorWidget);
delegate_seat!(MonitorWidget);
delegate_pointer!(MonitorWidget);
delegate_layer!(MonitorWidget);

delegate_registry!(MonitorWidget);

impl ProvidesRegistryState for MonitorWidget {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Ignore SIGPIPE so a closed socket becomes a normal EPIPE result, not a signal
    // This prevents the process from being killed when the compositor closes the connection
    unsafe { 
        libc::signal(libc::SIGPIPE, libc::SIG_IGN); 
    }
    
    // Initialize logger to write to /tmp/cosmic-monitor.log (shared with applet)
    use std::fs::OpenOptions;
    
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/cosmic-monitor.log")
        .expect("Failed to open log file");
    
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .init();
    
    log::info!("Starting COSMIC Monitor Widget");
    
    // Load configuration once (will be reloaded on changes inside the loop)
    let config_handler = cosmic_config::Config::new(
        "com.github.zoliviragh.CosmicMonitor",
        Config::VERSION,
    )?;
    
    let mut base_config = Config::get_entry(&config_handler).unwrap_or_default();
    
    log::info!("Widget starting with position: X={}, Y={}", base_config.widget_x, base_config.widget_y);
    log::info!("Weather enabled: {}, API key set: {}", base_config.show_weather, !base_config.weather_api_key.is_empty());
    log::info!("Notifications enabled: {}, section_order: {:?}", base_config.show_notifications, base_config.section_order);

    // RECONNECT LOOP - cycle through backoff intervals
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

        // INNER LOOP - one Wayland session
        'session: loop {
            let now = Instant::now();
            
            // First dispatch any pending events without blocking
            log::trace!("Dispatching events");
            if let Err(e) = event_queue.dispatch_pending(&mut widget) {
                log::error!("Error dispatching events: {}", e);
                
                // Check for broken pipe in error message - reconnect if so
                let error_str = e.to_string();
                if error_str.contains("Broken pipe") || error_str.contains("os error 32") {
                    log::warn!("Broken pipe during dispatch → reconnecting");
                    break 'session;
                }
                
                return Err(e.into());
            }
            log::trace!("Events dispatched");
            
            // Redraw when the clock second changes (synchronized with system time)
            let current_time = chrono::Local::now();
            
            // Subtract 1 second from the time we display to match system clock behavior
            // System clocks typically show the "current" second only after it's mostly elapsed
            let display_time = current_time - chrono::Duration::seconds(1);
            let current_second = display_time.format("%S").to_string();
            
            // Immediate redraw for notification interactions (independent of clock)
            if widget.force_redraw {
                widget.draw(&qh, display_time);
                widget.force_redraw = false;
                log::debug!("Immediate notification redraw triggered");
            }
            
            // Check if the second has changed since last draw for regular updates
            let should_redraw = if let Some(ref last_sec) = widget.last_drawn_second {
                &current_second != last_sec
            } else {
                true // First draw
            };
            
            if should_redraw {
                widget.draw(&qh, display_time);
                widget.last_drawn_second = Some(current_second);
            }
            
            // Check for config updates every 500ms
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
                        // Force a redraw
                        widget.draw(&qh, chrono::Local::now());
                    }
                }
            }

            // Aggressive heartbeat: force a roundtrip every 5 seconds to keep compositor connection alive
            // This actually waits for the compositor response, proving the connection works
            if now.duration_since(last_heartbeat) >= Duration::from_secs(5) {
                log::info!("Sending heartbeat roundtrip to compositor");
                if let Err(e) = event_queue.roundtrip(&mut widget) {
                    log::error!("Heartbeat roundtrip failed: {}", e);
                    
                    // Check for broken pipe - reconnect if so
                    let error_str = e.to_string();
                    if error_str.contains("Broken pipe") || error_str.contains("os error 32") {
                        log::warn!("Broken pipe on heartbeat → reconnecting");
                        break 'session;
                    }
                    
                    return Err(e.into());
                }
                last_heartbeat = now;
            }
            
            // CRITICAL: Always flush the connection to keep it alive
            // Must call flush at least a few times per second according to Wayland best practices
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
            
            // Small sleep to avoid busy-waiting while staying responsive
            thread::sleep(Duration::from_millis(16)); // ~60 FPS responsiveness

            if widget.exit {
                log::info!("Exit requested, shutting down");
                return Ok(());
            }
        } // end 'session

        // Backoff then reconnect
        let d = Duration::from_secs(backoff_secs.next().unwrap());
        log::info!("Reconnecting in {:?}...", d);
        thread::sleep(d);
        // loop continues...
    }
}
