// SPDX-License-Identifier: MPL-2.0

//! Widget implementation using Wayland layer-shell protocol
//! This bypasses the compositor's window management to achieve borderless rendering

mod config;
mod widget;

use config::Config;
use widget::{UtilizationMonitor, TemperatureMonitor, NetworkMonitor, WeatherMonitor};
use widget::renderer::{render_widget, RenderParams};
use widget::layout::calculate_widget_height;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use std::sync::Arc;
use std::time::Instant;

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
    last_update: Instant,
    
    /// Memory pool for rendering
    pool: Option<SlotPool>,
    
    /// Track last widget height for resizing
    last_height: u32,
    
    /// Mouse dragging state
    dragging: bool,
    drag_start_x: f64,
    drag_start_y: f64,
    
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
        self.draw(qh);
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
        self.draw(qh);
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
        // Layer-shell surfaces in COSMIC can't be interactively moved by users
        // Position is controlled via config file (widget_x, widget_y)
        // This handler is here for potential future use
        if !self.config.widget_movable {
            return;
        }

        for event in events {
            match event.kind {
                PointerEventKind::Press { button, .. } if button == 0x110 => {
                    self.dragging = true;
                    self.drag_start_x = event.position.0;
                    self.drag_start_y = event.position.1;
                }
                PointerEventKind::Release { button, .. } if button == 0x110 => {
                    self.dragging = false;
                }
                PointerEventKind::Motion { .. } if self.dragging => {
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
            last_update: Instant::now(),
            pool: None,
            last_height: WIDGET_HEIGHT,
            dragging: false,
            drag_start_x: 0.0,
            drag_start_y: 0.0,
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
        eprintln!("Setting layer surface margins: top={}, left={}", self.config.widget_y, self.config.widget_x);
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

        // Update monitoring modules
        self.utilization.update();
        self.temperature.update();
        self.network.update();
        
        // Update weather (has its own rate limiting - every 10 minutes)
        if self.config.show_weather {
            self.weather.update();
        }
    }

    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        let layer_surface = match &self.layer_surface {
            Some(ls) => ls.clone(),
            None => return,
        };

        self.update_system_stats();
        
        // Calculate dynamic height based on enabled components
        let width = WIDGET_WIDTH as i32;
        let height = calculate_widget_height(&self.config) as i32;
        let stride = width * 4;

        // Update layer surface size if height changed OR create pool if it doesn't exist
        if height as u32 != self.last_height || self.pool.is_none() {
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
        let gpu_usage = self.utilization.gpu_usage;
        let cpu_temp = self.temperature.cpu_temp;
        let gpu_temp = self.temperature.gpu_temp;
        let network_rx_rate = self.network.network_rx_rate;
        let network_tx_rate = self.network.network_tx_rate;
        let show_cpu = self.config.show_cpu;
        let show_memory = self.config.show_memory;
        let show_network = self.config.show_network;
        let show_disk = self.config.show_disk;
        let show_gpu = self.config.show_gpu;
        let show_cpu_temp = self.config.show_cpu_temp;
        let show_gpu_temp = self.config.show_gpu_temp;
        let show_clock = self.config.show_clock;
        let show_date = self.config.show_date;
        let show_percentages = self.config.show_percentages;
        let use_24hour_time = self.config.use_24hour_time;
        let use_circular_temp_display = self.config.use_circular_temp_display;
        let show_weather = self.config.show_weather;
        
        // Extract weather data
        let (weather_temp, weather_desc, weather_location, weather_icon) = if let Some(ref data) = self.weather.weather_data {
            (data.temperature, data.description.as_str(), data.location.as_str(), data.icon.as_str())
        } else {
            (0.0, "No data", "Unknown", "01d")
        };

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
            show_gpu,
            show_cpu_temp,
            show_gpu_temp,
            show_clock,
            show_date,
            show_percentages,
            use_24hour_time,
            use_circular_temp_display,
            show_weather,
            weather_temp,
            weather_desc,
            weather_location,
            weather_icon,
        };
        
        render_widget(canvas, params);

        // Attach the buffer to the surface
        layer_surface
            .wl_surface()
            .attach(Some(buffer.wl_buffer()), 0, 0);
        layer_surface.wl_surface().damage_buffer(0, 0, width, height);
        
        // Commit changes
        layer_surface.wl_surface().commit();
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
    // Load configuration
    let config_handler = cosmic_config::Config::new(
        "com.github.zoliviragh.CosmicMonitor",
        Config::VERSION,
    )?;
    
    let config = Config::get_entry(&config_handler).unwrap_or_default();
    
    eprintln!("Widget starting with position: X={}, Y={}", config.widget_x, config.widget_y);

    // Connect to Wayland
    let conn = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    // Create widget
    let mut widget = MonitorWidget::new(&globals, &qh, config, config_handler);
    widget.create_layer_surface(&qh);

    let mut last_draw = Instant::now();

    // Main event loop
    loop {
        let now = Instant::now();
        
        // Redraw every second for clock updates
        if now.duration_since(last_draw).as_secs() >= 1 {
            widget.draw(&qh);
            last_draw = now;
        }
        
        // Check for config updates every 500ms
        if now.duration_since(widget.last_config_check).as_millis() > 500 {
            widget.last_config_check = now;
            if let Ok(new_config) = Config::get_entry(&widget.config_handler) {
                // Only update if config actually changed
                if *widget.config != new_config {
                    // Update weather monitor if API key or location changed
                    if widget.config.weather_api_key != new_config.weather_api_key {
                        widget.weather.set_api_key(new_config.weather_api_key.clone());
                    }
                    if widget.config.weather_location != new_config.weather_location {
                        widget.weather.set_location(new_config.weather_location.clone());
                    }
                    
                    widget.config = Arc::new(new_config);
                    // Force a redraw
                    widget.draw(&qh);
                    last_draw = now; // Reset draw timer since we just drew
                }
            }
        }

        // Dispatch pending events without blocking
        event_queue.dispatch_pending(&mut widget)?;
        
        // Flush the connection
        event_queue.flush()?;
        
        // Sleep briefly to avoid busy-waiting
        std::thread::sleep(std::time::Duration::from_millis(100));

        if widget.exit {
            break;
        }
    }

    Ok(())
}
