// SPDX-License-Identifier: MPL-2.0

//! Widget implementation using Wayland layer-shell protocol
//! This bypasses the compositor's window management to achieve borderless rendering

mod config;

use config::Config;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use std::sync::Arc;
use std::time::Instant;
use sysinfo::{System, Networks};
use chrono::Local;

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

const WIDGET_WIDTH: u32 = 350;
const WIDGET_HEIGHT: u32 = 300;

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
    
    /// System monitoring
    sys: System,
    networks: Networks,
    cpu_usage: f32,
    memory_usage: f32,
    memory_total: u64,
    memory_used: u64,
    network_rx_bytes: u64,
    network_tx_bytes: u64,
    network_rx_rate: f64,
    network_tx_rate: f64,
    last_update: Instant,
    
    /// Memory pool for rendering
    pool: Option<SlotPool>,
    
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
            sys: System::new_all(),
            networks: Networks::new_with_refreshed_list(),
            cpu_usage: 0.0,
            memory_usage: 0.0,
            memory_total: 0,
            memory_used: 0,
            network_rx_bytes: 0,
            network_tx_bytes: 0,
            network_rx_rate: 0.0,
            network_tx_rate: 0.0,
            last_update: Instant::now(),
            pool: None,
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

        // Update CPU usage
        self.sys.refresh_cpu_all();
        self.cpu_usage = self.sys.global_cpu_usage();

        // Update memory usage
        self.sys.refresh_memory();
        self.memory_used = self.sys.used_memory();
        self.memory_total = self.sys.total_memory();
        self.memory_usage = if self.memory_total > 0 {
            (self.memory_used as f32 / self.memory_total as f32) * 100.0
        } else {
            0.0
        };

        // Update network statistics
        self.networks.refresh();
        let mut total_rx = 0;
        let mut total_tx = 0;
        for (_interface_name, network) in &self.networks {
            total_rx += network.received();
            total_tx += network.transmitted();
        }
        
        if self.network_rx_bytes > 0 {
            self.network_rx_rate = (total_rx - self.network_rx_bytes) as f64 / elapsed;
            self.network_tx_rate = (total_tx - self.network_tx_bytes) as f64 / elapsed;
        }
        self.network_rx_bytes = total_rx;
        self.network_tx_bytes = total_tx;
    }

    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        let layer_surface = match &self.layer_surface {
            Some(ls) => ls.clone(),
            None => return,
        };

        self.update_system_stats();
        
        let width = WIDGET_WIDTH as i32;
        let height = WIDGET_HEIGHT as i32;
        let stride = width * 4;

        // Create pool if it doesn't exist
        if self.pool.is_none() {
            self.pool = Some(SlotPool::new(width as usize * height as usize * 4, &self.shm_state)
                .expect("Failed to create pool"));
        }

        // Store the data we need for rendering
        let cpu_usage = self.cpu_usage;
        let memory_usage = self.memory_usage;
        let memory_used = self.memory_used;
        let memory_total = self.memory_total;
        let network_rx_rate = self.network_rx_rate;
        let network_tx_rate = self.network_tx_rate;
        let show_cpu = self.config.show_cpu;
        let show_memory = self.config.show_memory;
        let show_network = self.config.show_network;
        let show_disk = self.config.show_disk;
        let show_gpu = self.config.show_gpu;
        let show_clock = self.config.show_clock;
        let show_date = self.config.show_date;
        let show_percentages = self.config.show_percentages;

        let pool = self.pool.as_mut().unwrap();

        let (buffer, canvas) = pool
            .create_buffer(width, height, stride, wl_shm::Format::Argb8888)
            .expect("Failed to create buffer");

        // Use Cairo for rendering
        render_widget(
            canvas,
            cpu_usage,
            memory_usage,
            memory_used,
            memory_total,
            network_rx_rate,
            network_tx_rate,
            show_cpu,
            show_memory,
            show_network,
            show_disk,
            show_gpu,
            show_clock,
            show_date,
            show_percentages,
        );

        // Attach the buffer to the surface
        layer_surface
            .wl_surface()
            .attach(Some(buffer.wl_buffer()), 0, 0);
        layer_surface.wl_surface().damage_buffer(0, 0, width, height);
        
        // Commit changes
        layer_surface.wl_surface().commit();
    }
}

/// Draw a CPU icon (simple chip representation)
fn draw_cpu_icon(cr: &cairo::Context, x: f64, y: f64, size: f64) {
    // Draw chip body
    cr.rectangle(x, y, size, size);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    
    // Draw pins on sides
    let pin_length = size * 0.2;
    let pin_spacing = size / 3.0;
    
    // Left pins
    for i in 0..3 {
        let py = y + pin_spacing * (i as f64 + 0.5);
        cr.move_to(x, py);
        cr.line_to(x - pin_length, py);
    }
    
    // Right pins
    for i in 0..3 {
        let py = y + pin_spacing * (i as f64 + 0.5);
        cr.move_to(x + size, py);
        cr.line_to(x + size + pin_length, py);
    }
    
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.stroke().expect("Failed to stroke");
}

/// Draw a RAM icon (simple memory chip representation)
fn draw_ram_icon(cr: &cairo::Context, x: f64, y: f64, size: f64) {
    // Draw memory stick body
    cr.rectangle(x, y + size * 0.2, size, size * 0.8);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    
    // Draw notch at top
    let notch_width = size * 0.3;
    let notch_x = x + (size - notch_width) / 2.0;
    cr.rectangle(notch_x, y, notch_width, size * 0.2);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    
    // Draw chips on the body
    let chip_size = size * 0.15;
    for i in 0..3 {
        let chip_y = y + size * 0.3 + i as f64 * size * 0.22;
        cr.rectangle(x + size * 0.15, chip_y, chip_size, chip_size);
        cr.rectangle(x + size * 0.55, chip_y, chip_size, chip_size);
    }
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(1.5);
    cr.stroke().expect("Failed to stroke");
}

/// Draw a GPU icon (graphics card representation)
fn draw_gpu_icon(cr: &cairo::Context, x: f64, y: f64, size: f64) {
    // Draw GPU card body
    cr.rectangle(x, y + size * 0.3, size * 1.3, size * 0.7);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    
    // Draw fan (circle)
    cr.arc(x + size * 0.65, y + size * 0.65, size * 0.25, 0.0, 2.0 * std::f64::consts::PI);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke().expect("Failed to stroke");
    
    // Draw PCIe connector
    for i in 0..3 {
        let connector_x = x + i as f64 * size * 0.15;
        cr.rectangle(connector_x, y, size * 0.1, size * 0.25);
    }
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(1.5);
    cr.stroke().expect("Failed to stroke");
}

/// Draw a horizontal progress bar
fn draw_progress_bar(cr: &cairo::Context, x: f64, y: f64, width: f64, height: f64, percentage: f32) {
    // Draw background
    cr.rectangle(x, y, width, height);
    cr.set_source_rgba(0.2, 0.2, 0.2, 0.7);
    cr.fill().expect("Failed to fill");
    
    // Draw border
    cr.rectangle(x, y, width, height);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(1.0);
    cr.stroke().expect("Failed to stroke");
    
    // Draw filled portion
    let fill_width = width * (percentage / 100.0).min(1.0) as f64;
    if fill_width > 0.0 {
        cr.rectangle(x + 1.0, y + 1.0, fill_width - 2.0, height - 2.0);
        
        // Gradient fill based on percentage
        let pattern = cairo::LinearGradient::new(x, y, x + width, y);
        if percentage < 50.0 {
            pattern.add_color_stop_rgb(0.0, 0.2, 0.8, 0.2); // Green
            pattern.add_color_stop_rgb(1.0, 0.4, 0.9, 0.4);
        } else if percentage < 80.0 {
            pattern.add_color_stop_rgb(0.0, 0.8, 0.8, 0.2); // Yellow
            pattern.add_color_stop_rgb(1.0, 0.9, 0.9, 0.4);
        } else {
            pattern.add_color_stop_rgb(0.0, 0.9, 0.2, 0.2); // Red
            pattern.add_color_stop_rgb(1.0, 1.0, 0.4, 0.4);
        }
        
        cr.set_source(&pattern).expect("Failed to set source");
        cr.fill().expect("Failed to fill");
    }
}

fn render_widget(
    canvas: &mut [u8],
    cpu_usage: f32,
    memory_usage: f32,
    memory_used: u64,
    memory_total: u64,
    network_rx_rate: f64,
    network_tx_rate: f64,
    show_cpu: bool,
    show_memory: bool,
    show_network: bool,
    show_disk: bool,
    show_gpu: bool,
    show_clock: bool,
    show_date: bool,
    show_percentages: bool,
) {
    // Use unsafe to extend the lifetime for Cairo
    // This is safe because the surface doesn't outlive the canvas buffer
    let surface = unsafe {
        let ptr = canvas.as_mut_ptr();
        let len = canvas.len();
        let static_slice: &'static mut [u8] = std::slice::from_raw_parts_mut(ptr, len);
        
        cairo::ImageSurface::create_for_data(
            static_slice,
            cairo::Format::ARgb32,
            WIDGET_WIDTH as i32,
            WIDGET_HEIGHT as i32,
            WIDGET_WIDTH as i32 * 4,
        )
        .expect("Failed to create cairo surface")
    };

    {
        let cr = cairo::Context::new(&surface).expect("Failed to create cairo context");

        // Clear background to fully transparent
        cr.save().expect("Failed to save");
        cr.set_operator(cairo::Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        cr.paint().expect("Failed to clear");
        cr.restore().expect("Failed to restore");

        // Set up Pango for text rendering
        let layout = pangocairo::functions::create_layout(&cr);
        
        // Track vertical position
        let mut y_pos = 10.0;
        
        // Get current date/time
        let now = chrono::Local::now();
        
        if show_clock {
            // Draw large time (HH:MM)
            let time_str = now.format("%H:%M").to_string();
            let font_desc = pango::FontDescription::from_string("Ubuntu Bold 48");
            layout.set_font_description(Some(&font_desc));
            layout.set_text(&time_str);
            
            // White text with black outline
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.move_to(10.0, y_pos);
            
            // Draw outline
            cr.set_line_width(3.0);
            pangocairo::functions::layout_path(&cr, &layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            
            // Fill with white
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
            
            // Get width of the time text to position seconds correctly
            let (time_width, _) = layout.pixel_size();
            
            // Draw seconds (:SS) slightly smaller and raised
            let seconds_str = now.format(":%S").to_string();
            let font_desc = pango::FontDescription::from_string("Ubuntu Bold 28");
            layout.set_font_description(Some(&font_desc));
            layout.set_text(&seconds_str);
            
            cr.move_to(10.0 + time_width as f64, y_pos + 5.0); // Position after HH:MM, slightly lower
            pangocairo::functions::layout_path(&cr, &layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
            
            y_pos += 70.0; // Move down after clock
        }
        
        if show_date {
            // Draw date below with more spacing
            let date_str = now.format("%A, %d %B %Y").to_string();
            let font_desc = pango::FontDescription::from_string("Ubuntu 16");
            layout.set_font_description(Some(&font_desc));
            layout.set_text(&date_str);
            
            cr.move_to(10.0, y_pos);
            pangocairo::functions::layout_path(&cr, &layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
            
            y_pos += 35.0; // Move down after date
        }
        
        // Add spacing before stats if we showed clock or date
        if show_clock || show_date {
            y_pos += 20.0;
        } else {
            y_pos = 10.0; // Start at top if no clock/date
        }
        
        // Start system stats
        let mut y = y_pos;
        let icon_size = 20.0;
        let bar_width = 200.0;
        let bar_height = 12.0;

        // Draw stats with outline effect
        let font_desc = pango::FontDescription::from_string("Ubuntu 12");
        layout.set_font_description(Some(&font_desc));
        cr.set_line_width(2.0);
        
        if show_cpu {
            // Draw CPU icon
            draw_cpu_icon(&cr, 10.0, y - 2.0, icon_size);
            
            // Draw CPU label
            layout.set_text("CPU:");
            cr.move_to(10.0 + icon_size + 10.0, y);
            pangocairo::functions::layout_path(&cr, &layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
            
            // Draw progress bar
            draw_progress_bar(&cr, 90.0, y, bar_width, bar_height, cpu_usage);
            
            // Draw CPU percentage only if show_percentages is enabled
            if show_percentages {
                let cpu_text = format!("{:.1}%", cpu_usage);
                layout.set_text(&cpu_text);
                cr.move_to(300.0, y);
                pangocairo::functions::layout_path(&cr, &layout);
                cr.set_source_rgb(0.0, 0.0, 0.0);
                cr.stroke_preserve().expect("Failed to stroke");
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.fill().expect("Failed to fill");
            }
            
            y += 30.0;
        }

        if show_memory {
            // Draw RAM icon
            draw_ram_icon(&cr, 10.0, y - 2.0, icon_size);
            
            // Draw Memory label
            layout.set_text("RAM:");
            cr.move_to(10.0 + icon_size + 10.0, y);
            pangocairo::functions::layout_path(&cr, &layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
            
            // Draw progress bar first
            draw_progress_bar(&cr, 90.0, y, bar_width, bar_height, memory_usage);
            
            // Draw memory percentage only if show_percentages is enabled
            if show_percentages {
                let mem_text = format!("{:.1}%", memory_usage);
                layout.set_text(&mem_text);
                cr.move_to(300.0, y); // Position after the bar
                pangocairo::functions::layout_path(&cr, &layout);
                cr.set_source_rgb(0.0, 0.0, 0.0);
                cr.stroke_preserve().expect("Failed to stroke");
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.fill().expect("Failed to fill");
            }
            
            y += 30.0;
        }

        if show_gpu {
            // Draw GPU icon
            draw_gpu_icon(&cr, 10.0, y - 2.0, icon_size);
            
            // Draw GPU label
            layout.set_text("GPU:");
            cr.move_to(10.0 + icon_size + 10.0, y);
            pangocairo::functions::layout_path(&cr, &layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
            
            // Draw progress bar
            let gpu_usage = 0.0; // TODO: Implement actual GPU monitoring
            draw_progress_bar(&cr, 90.0, y, bar_width, bar_height, gpu_usage);
            
            // Draw GPU percentage only if show_percentages is enabled (placeholder - needs nvtop/radeontop integration)
            if show_percentages {
                let gpu_text = format!("{:.1}%", gpu_usage);
                layout.set_text(&gpu_text);
                cr.move_to(300.0, y);
                pangocairo::functions::layout_path(&cr, &layout);
                cr.set_source_rgb(0.0, 0.0, 0.0);
                cr.stroke_preserve().expect("Failed to stroke");
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.fill().expect("Failed to fill");
            }
            
            y += 30.0;
        }

        if show_network {
            layout.set_text(&format!("Network ↓: {:.1} KB/s", network_rx_rate / 1024.0));
            cr.move_to(10.0, y);
            pangocairo::functions::layout_path(&cr, &layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
            y += 25.0;

            layout.set_text(&format!("Network ↑: {:.1} KB/s", network_tx_rate / 1024.0));
            cr.move_to(10.0, y);
            pangocairo::functions::layout_path(&cr, &layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
            y += 25.0;
        }

        if show_disk {
            layout.set_text("Disk Read: 0.0 KB/s");
            cr.move_to(10.0, y);
            pangocairo::functions::layout_path(&cr, &layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
            y += 25.0;

            layout.set_text("Disk Write: 0.0 KB/s");
            cr.move_to(10.0, y);
            pangocairo::functions::layout_path(&cr, &layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
        }
    }
    
    // Ensure Cairo surface is flushed
    surface.flush();
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
