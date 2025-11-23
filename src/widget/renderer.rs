// SPDX-License-Identifier: MPL-2.0

//! Rendering module for the widget
//! Contains the main rendering logic and helper functions

use cairo;
use pango;
use pangocairo;

use super::utilization::{draw_cpu_icon, draw_ram_icon, draw_gpu_icon, draw_progress_bar};
use super::temperature::draw_temp_circle;
use super::weather::draw_weather_icon;
use super::storage::DiskInfo;
use super::battery::BatteryDevice;
use crate::config::WidgetSection;

/// Parameters for rendering the widget
pub struct RenderParams<'a> {
    pub width: i32,
    pub height: i32,
    pub cpu_usage: f32,
    pub memory_usage: f32,
    pub gpu_usage: f32,
    pub cpu_temp: f32,
    pub gpu_temp: f32,
    pub network_rx_rate: f64,
    pub network_tx_rate: f64,
    pub show_cpu: bool,
    pub show_memory: bool,
    pub show_network: bool,
    pub show_disk: bool,
    pub show_storage: bool,
    pub show_gpu: bool,
    pub show_cpu_temp: bool,
    pub show_gpu_temp: bool,
    pub show_clock: bool,
    pub show_date: bool,
    pub show_percentages: bool,
    pub use_24hour_time: bool,
    pub use_circular_temp_display: bool,
    pub show_weather: bool,
    pub show_battery: bool,
    pub enable_solaar_integration: bool,
    pub weather_temp: f32,
    pub weather_desc: &'a str,
    pub weather_location: &'a str,
    pub weather_icon: &'a str,
    pub disk_info: &'a [DiskInfo],
    pub battery_devices: &'a [BatteryDevice],
    pub section_order: &'a [WidgetSection],
}

/// Main rendering function for the widget
pub fn render_widget(canvas: &mut [u8], params: RenderParams) {
    // Use unsafe to extend the lifetime for Cairo
    // This is safe because the surface doesn't outlive the canvas buffer
    let surface = unsafe {
        let ptr = canvas.as_mut_ptr();
        let len = canvas.len();
        let static_slice: &'static mut [u8] = std::slice::from_raw_parts_mut(ptr, len);
        
        cairo::ImageSurface::create_for_data(
            static_slice,
            cairo::Format::ARgb32,
            params.width,
            params.height,
            params.width * 4,
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
        
        // Render sections
        if params.show_clock || params.show_date {
            y_pos = render_datetime(&cr, &layout, y_pos, params.show_clock, params.show_date, params.use_24hour_time);
            y_pos += 20.0; // Spacing after datetime
        } else {
            y_pos = 10.0; // Start at top if no clock/date
        }
        
        // Render sections in the configured order
        for section in params.section_order {
            match section {
                WidgetSection::Utilization => {
                    if params.show_cpu || params.show_memory || params.show_gpu {
                        y_pos = render_utilization(&cr, &layout, y_pos, &params);
                    }
                }
                WidgetSection::Temperatures => {
                    if params.show_cpu_temp || params.show_gpu_temp {
                        y_pos += 10.0; // Spacing before temperature section
                        y_pos = render_temperatures(&cr, &layout, y_pos, &params);
                    }
                }
                WidgetSection::Storage => {
                    if params.show_storage {
                        y_pos += 10.0; // Spacing before storage section
                        y_pos = render_storage(&cr, &layout, y_pos, params.disk_info, params.show_percentages);
                    }
                }
                WidgetSection::Battery => {
                    if params.show_battery {
                        y_pos += 10.0; // Spacing before battery section
                        y_pos = render_battery_section(
                            &cr,
                            &layout,
                            y_pos,
                            params.battery_devices,
                            params.enable_solaar_integration,
                        );
                    }
                }
                WidgetSection::Weather => {
                    if params.show_weather {
                        y_pos += 10.0; // Spacing before weather section
                        y_pos = render_weather(&cr, &layout, y_pos, &params);
                    }
                }
            }
        }
        
        // Render network and disk (not yet in reorderable sections)
        if params.show_network {
            y_pos = render_network(&cr, &layout, y_pos, params.network_rx_rate, params.network_tx_rate);
        }
        
        if params.show_disk {
            y_pos = render_disk(&cr, &layout, y_pos);
        }
    }
    
    // Ensure Cairo surface is flushed
    surface.flush();
}

/// Render date/time section
fn render_datetime(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    show_clock: bool,
    show_date: bool,
    use_24hour_time: bool,
) -> f64 {
    let mut y_pos = y_start;
    let now = chrono::Local::now();
    
    if show_clock {
        // Draw large time (HH:MM or h:MM based on format)
        let time_str = if use_24hour_time {
            now.format("%H:%M").to_string()
        } else {
            now.format("%-I:%M").to_string()
        };
        let font_desc = pango::FontDescription::from_string("Ubuntu Bold 48");
        layout.set_font_description(Some(&font_desc));
        layout.set_text(&time_str);
        
        // White text with black outline
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.move_to(10.0, y_pos);
        
        // Draw outline
        cr.set_line_width(3.0);
        pangocairo::functions::layout_path(cr, layout);
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
        
        cr.move_to(10.0 + time_width as f64, y_pos + 5.0);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        // For 12-hour format, add AM/PM indicator
        if !use_24hour_time {
            let ampm_str = now.format(" %p").to_string();
            let font_desc = pango::FontDescription::from_string("Ubuntu Bold 20");
            layout.set_font_description(Some(&font_desc));
            layout.set_text(&ampm_str);
            
            let (seconds_width, _) = layout.pixel_size();
            cr.move_to(10.0 + time_width as f64 + seconds_width as f64, y_pos + 10.0);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
        }
        
        y_pos += 70.0; // Move down after clock
    }
    
    if show_date {
        // Draw date below with more spacing
        let date_str = now.format("%A, %d %B %Y").to_string();
        let font_desc = pango::FontDescription::from_string("Ubuntu 16");
        layout.set_font_description(Some(&font_desc));
        layout.set_text(&date_str);
        
        cr.move_to(10.0, y_pos);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        y_pos += 35.0; // Move down after date
    }
    
    y_pos
}

/// Render utilization section (CPU, RAM, GPU)
fn render_utilization(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    params: &RenderParams,
) -> f64 {
    let mut y = y_start;
    let icon_size = 20.0;
    let bar_width = 200.0;
    let bar_height = 12.0;
    
    // Draw section header
    let header_font = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&header_font));
    layout.set_text("Utilization");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    
    y += 35.0;
    
    // Set normal font for items
    let font_desc = pango::FontDescription::from_string("Ubuntu 12");
    layout.set_font_description(Some(&font_desc));
    cr.set_line_width(2.0);
    
    if params.show_cpu {
        draw_cpu_icon(cr, 10.0, y - 2.0, icon_size);
        
        layout.set_text("CPU:");
        cr.move_to(10.0 + icon_size + 10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        draw_progress_bar(cr, 90.0, y, bar_width, bar_height, params.cpu_usage);
        
        if params.show_percentages {
            let cpu_text = format!("{:.1}%", params.cpu_usage);
            layout.set_text(&cpu_text);
            cr.move_to(300.0, y);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
        }
        
        y += 30.0;
    }
    
    if params.show_memory {
        draw_ram_icon(cr, 10.0, y - 2.0, icon_size);
        
        layout.set_text("RAM:");
        cr.move_to(10.0 + icon_size + 10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        draw_progress_bar(cr, 90.0, y, bar_width, bar_height, params.memory_usage);
        
        if params.show_percentages {
            let mem_text = format!("{:.1}%", params.memory_usage);
            layout.set_text(&mem_text);
            cr.move_to(300.0, y);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
        }
        
        y += 30.0;
    }
    
    if params.show_gpu {
        draw_gpu_icon(cr, 10.0, y - 2.0, icon_size);
        
        layout.set_text("GPU:");
        cr.move_to(10.0 + icon_size + 10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        draw_progress_bar(cr, 90.0, y, bar_width, bar_height, params.gpu_usage);
        
        if params.show_percentages {
            let gpu_text = format!("{:.1}%", params.gpu_usage);
            layout.set_text(&gpu_text);
            cr.move_to(300.0, y);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
        }
        
        y += 30.0;
    }
    
    y
}

/// Render temperature section
fn render_temperatures(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    params: &RenderParams,
) -> f64 {
    let mut y = y_start;
    
    // Draw section header
    let font_desc = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&font_desc));
    layout.set_text("Temperatures");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 35.0;
    
    if params.use_circular_temp_display {
        y = render_circular_temps(cr, layout, y, params);
    } else {
        y = render_text_temps(cr, layout, y, params);
    }
    
    y
}

/// Render circular temperature gauges
fn render_circular_temps(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    params: &RenderParams,
) -> f64 {
    let y = y_start;
    let circle_radius = 25.0;
    let circle_diameter = circle_radius * 2.0;
    let spacing = 20.0;
    let mut x_offset = 15.0;
    let max_temp = 100.0;
    
    if params.show_cpu_temp {
        draw_temp_circle(cr, x_offset, y, circle_radius, params.cpu_temp, max_temp);
        
        // Temperature value in center
        let temp_text = if params.cpu_temp > 0.0 {
            format!("{:.0}°", params.cpu_temp)
        } else {
            "N/A".to_string()
        };
        let font_desc = pango::FontDescription::from_string("Ubuntu Bold 12");
        layout.set_font_description(Some(&font_desc));
        layout.set_text(&temp_text);
        let (text_width, text_height) = layout.pixel_size();
        cr.move_to(
            x_offset + circle_radius - text_width as f64 / 2.0,
            y + circle_radius - text_height as f64 / 2.0
        );
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        // "CPU" label below circle
        let label_font = pango::FontDescription::from_string("Ubuntu 10");
        layout.set_font_description(Some(&label_font));
        layout.set_text("CPU");
        let (label_width, _) = layout.pixel_size();
        cr.move_to(
            x_offset + circle_radius - label_width as f64 / 2.0,
            y + circle_diameter + 2.0
        );
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        x_offset += circle_diameter + spacing;
    }
    
    if params.show_gpu_temp {
        draw_temp_circle(cr, x_offset, y, circle_radius, params.gpu_temp, max_temp);
        
        // Temperature value in center
        let temp_text = if params.gpu_temp > 0.0 {
            format!("{:.0}°", params.gpu_temp)
        } else {
            "N/A".to_string()
        };
        let font_desc = pango::FontDescription::from_string("Ubuntu Bold 12");
        layout.set_font_description(Some(&font_desc));
        layout.set_text(&temp_text);
        let (text_width, text_height) = layout.pixel_size();
        cr.move_to(
            x_offset + circle_radius - text_width as f64 / 2.0,
            y + circle_radius - text_height as f64 / 2.0
        );
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        
        // "GPU" label below circle
        let label_font = pango::FontDescription::from_string("Ubuntu 10");
        layout.set_font_description(Some(&label_font));
        layout.set_text("GPU");
        let (label_width, _) = layout.pixel_size();
        cr.move_to(
            x_offset + circle_radius - label_width as f64 / 2.0,
            y + circle_diameter + 2.0
        );
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
    }
    
    y + circle_diameter + 15.0
}

/// Render text-based temperatures
fn render_text_temps(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    params: &RenderParams,
) -> f64 {
    let mut y = y_start;
    let font_desc = pango::FontDescription::from_string("Ubuntu 14");
    layout.set_font_description(Some(&font_desc));
    
    if params.show_cpu_temp {
        if params.cpu_temp > 0.0 {
            layout.set_text(&format!("  CPU: {:.1}°C", params.cpu_temp));
        } else {
            layout.set_text("  CPU: N/A");
        }
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 25.0;
    }
    
    if params.show_gpu_temp {
        if params.gpu_temp > 0.0 {
            layout.set_text(&format!("  GPU: {:.1}°C", params.gpu_temp));
        } else {
            layout.set_text("  GPU: N/A");
        }
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 25.0;
    }
    
    y
}

/// Render network stats
fn render_network(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    rx_rate: f64,
    tx_rate: f64,
) -> f64 {
    let mut y = y_start;
    
    layout.set_text(&format!("Network ↓: {:.1} KB/s", rx_rate / 1024.0));
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 25.0;
    
    layout.set_text(&format!("Network ↑: {:.1} KB/s", tx_rate / 1024.0));
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 25.0;
    
    y
}

/// Render disk stats
fn render_disk(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
) -> f64 {
    let mut y = y_start;
    
    layout.set_text("Disk Read: 0.0 KB/s");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 25.0;
    
    layout.set_text("Disk Write: 0.0 KB/s");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 25.0;
    
    y
}

/// Temporary battery section placeholder until Solaar integration is implemented
fn render_battery_section(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    devices: &[BatteryDevice],
    enable_solaar_integration: bool,
) -> f64 {
    let mut y = y_start;

    // Section header
    let header_font = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&header_font));
    layout.set_text("Battery");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 35.0;

    // Simple text to indicate Solaar integration state
    let font_desc = pango::FontDescription::from_string("Ubuntu 12");
    layout.set_font_description(Some(&font_desc));

    if !enable_solaar_integration {
        layout.set_text("Solaar integration disabled");
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 25.0;
        return y;
    }

    if devices.is_empty() {
        layout.set_text("No Solaar devices detected");
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 25.0;
        return y;
    }

    let icon_size = 24.0;

    for device in devices {
        // Draw device name
        layout.set_text(&device.name);
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 28.0;

        if !device.is_connected {
            // Device is disconnected - show disconnected icon
            draw_disconnected_icon(cr, 10.0, y - 2.0, icon_size);
            
            // Draw "Disconnected" text
            layout.set_text("Disconnected");
            cr.move_to(10.0 + icon_size + 8.0, y - 2.0);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(0.7, 0.7, 0.7);
            cr.fill().expect("Failed to fill");
            
            y += 38.0;
        } else if device.is_loading {
            // Device is connected but loading - show disconnected icon with "Connecting..." text
            draw_disconnected_icon(cr, 10.0, y - 2.0, icon_size);
            
            // Draw "Connecting..." text
            layout.set_text("Connecting...");
            cr.move_to(10.0 + icon_size + 8.0, y - 2.0);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(0.7, 0.7, 0.7);
            cr.fill().expect("Failed to fill");
            
            y += 38.0;
        } else if let Some(level) = device.level {
            // Check if device is charging (use lowercase and check for "recharging" or starts with "charging")
            let is_charging = device.status.as_deref()
                .map(|s| {
                    let lower = s.to_lowercase();
                    lower.starts_with("charging") || lower.starts_with("recharging")
                })
                .unwrap_or(false);
            
            // Draw vertical battery icon
            draw_battery_icon(cr, 10.0, y - 2.0, icon_size, level);
            
            // If charging, draw a lightning bolt overlay
            if is_charging {
                draw_charging_indicator(cr, 10.0, y - 2.0, icon_size);
            }

            // Draw percentage text next to battery with charging indicator
            let percentage_text = if is_charging {
                format!("{}% ⚡", level)
            } else {
                format!("{}%", level)
            };
            layout.set_text(&percentage_text);
            cr.move_to(10.0 + icon_size + 8.0, y - 2.0);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");

            y += 38.0; // Increased spacing between devices
        } else {
            // No battery level available
            layout.set_text("  Battery: N/A");
            cr.move_to(10.0, y);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
            y += 38.0; // Increased spacing between devices
        }
    }

    y
}

/// Draw a vertical battery icon with fill level
fn draw_battery_icon(cr: &cairo::Context, x: f64, y: f64, size: f64, level: u8) {
    let (r, g, b) = get_battery_color(level);
    let body_height = size;
    let body_width = size * 0.6;
    let terminal_height = size * 0.1;
    let terminal_width = body_width * 0.4;
    
    // Battery terminal (small rectangle on top)
    let terminal_x = x + (body_width - terminal_width) / 2.0;
    cr.rectangle(terminal_x, y, terminal_width, terminal_height);
    cr.set_source_rgb(0.6, 0.6, 0.6);
    cr.fill_preserve().expect("Failed to fill");
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(1.0);
    cr.stroke().expect("Failed to stroke");
    
    // Battery body (vertical rectangle)
    let body_y = y + terminal_height;
    cr.rectangle(x, body_y, body_width, body_height);
    cr.set_source_rgb(0.2, 0.2, 0.2);
    cr.fill_preserve().expect("Failed to fill");
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(1.5);
    cr.stroke().expect("Failed to stroke");
    
    // Fill level indicator inside battery (from bottom up)
    if level > 0 {
        let fill_height = (body_height - 4.0) * (level as f64 / 100.0);
        let fill_y = body_y + body_height - 2.0 - fill_height;
        cr.rectangle(x + 2.0, fill_y, body_width - 4.0, fill_height);
        cr.set_source_rgb(r, g, b);
        cr.fill().expect("Failed to fill");
    }
}

/// Draw a disconnected/loading icon for battery devices
fn draw_disconnected_icon(cr: &cairo::Context, x: f64, y: f64, size: f64) {
    // Draw a battery outline in gray with a slash through it
    let body_height = size;
    let body_width = size * 0.6;
    let terminal_height = size * 0.1;
    let terminal_width = body_width * 0.4;
    
    // Battery terminal (gray)
    let terminal_x = x + (body_width - terminal_width) / 2.0;
    cr.rectangle(terminal_x, y, terminal_width, terminal_height);
    cr.set_source_rgb(0.5, 0.5, 0.5);
    cr.fill_preserve().expect("Failed to fill");
    cr.set_source_rgb(0.3, 0.3, 0.3);
    cr.set_line_width(1.0);
    cr.stroke().expect("Failed to stroke");
    
    // Battery body (gray outline, no fill)
    let body_y = y + terminal_height;
    cr.rectangle(x, body_y, body_width, body_height);
    cr.set_source_rgb(0.5, 0.5, 0.5);
    cr.set_line_width(1.5);
    cr.stroke().expect("Failed to stroke");
    
    // Draw diagonal slash to indicate disconnected
    cr.move_to(x, body_y);
    cr.line_to(x + body_width, body_y + body_height);
    cr.set_source_rgb(0.8, 0.3, 0.3);
    cr.set_line_width(2.0);
    cr.stroke().expect("Failed to stroke");
}

/// Draw a charging indicator (lightning bolt) overlay on battery icon
fn draw_charging_indicator(cr: &cairo::Context, x: f64, y: f64, size: f64) {
    let body_width = size * 0.6;
    let body_height = size;
    let terminal_height = size * 0.1;
    let body_y = y + terminal_height;
    
    // Draw lightning bolt in center of battery
    let bolt_x = x + body_width / 2.0;
    let bolt_y = body_y + body_height * 0.2;
    let bolt_height = body_height * 0.6;
    let bolt_width = body_width * 0.4;
    
    cr.save().expect("Failed to save");
    cr.set_source_rgba(1.0, 1.0, 0.0, 0.9); // Yellow with slight transparency
    cr.set_line_width(2.0);
    
    // Draw lightning bolt shape
    cr.move_to(bolt_x, bolt_y);
    cr.line_to(bolt_x - bolt_width / 3.0, bolt_y + bolt_height / 2.0);
    cr.line_to(bolt_x, bolt_y + bolt_height / 2.0);
    cr.line_to(bolt_x - bolt_width / 3.0, bolt_y + bolt_height);
    cr.stroke().expect("Failed to stroke");
    
    cr.move_to(bolt_x, bolt_y + bolt_height / 2.0);
    cr.line_to(bolt_x + bolt_width / 3.0, bolt_y);
    cr.stroke().expect("Failed to stroke");
    
    cr.restore().expect("Failed to restore");
}

/// Get RGB color based on battery level
fn get_battery_color(level: u8) -> (f64, f64, f64) {
    if level > 60 {
        (0.0, 0.8, 0.0) // Green
    } else if level > 30 {
        (1.0, 0.8, 0.0) // Yellow/Orange
    } else if level > 15 {
        (1.0, 0.5, 0.0) // Orange
    } else {
        (1.0, 0.0, 0.0) // Red
    }
}

/// Render weather section
fn render_weather(
    cr: &cairo::Context,
    layout: &pango::Layout,
    y_start: f64,
    params: &RenderParams,
) -> f64 {
    let mut y = y_start;
    
    // Section header
    let header_font = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&header_font));
    layout.set_text("Weather");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 35.0;
    
    // Draw weather icon
    let icon_size = 40.0;
    draw_weather_icon(cr, 10.0, y, icon_size, params.weather_icon);
    
    // Weather info to the right of icon
    let info_x = 60.0;
    let font_desc = pango::FontDescription::from_string("Ubuntu 14");
    layout.set_font_description(Some(&font_desc));
    
    // Temperature
    if !params.weather_temp.is_nan() {
        layout.set_text(&format!("{:.1}°C", params.weather_temp));
    } else {
        layout.set_text("N/A");
    }
    cr.move_to(info_x, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    
    // Description
    layout.set_text(params.weather_desc);
    cr.move_to(info_x, y + 20.0);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    
    // Location
    let location_font = pango::FontDescription::from_string("Ubuntu 12");
    layout.set_font_description(Some(&location_font));
    layout.set_text(params.weather_location);
    cr.move_to(info_x, y + 45.0);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(0.7, 0.7, 0.7);
    cr.fill().expect("Failed to fill");
    
    y + 70.0 // Return updated y position
}

/// Render storage/disk usage section
fn render_storage(cr: &cairo::Context, layout: &pango::Layout, y: f64, disk_info: &[DiskInfo], show_percentages: bool) -> f64 {
    let mut y = y;
    let bar_width = 200.0;
    let bar_height = 12.0;
    
    // Section header
    let header_font = pango::FontDescription::from_string("Ubuntu Bold 14");
    layout.set_font_description(Some(&header_font));
    layout.set_text("Storage");
    cr.move_to(10.0, y);
    pangocairo::functions::layout_path(cr, layout);
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_line_width(2.0);
    cr.stroke_preserve().expect("Failed to stroke");
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.fill().expect("Failed to fill");
    y += 35.0; // Spacing after header
    
    // Draw each disk
    let font_desc = pango::FontDescription::from_string("Ubuntu 12");
    layout.set_font_description(Some(&font_desc));
    cr.set_line_width(2.0);
    
    for disk in disk_info {
        // Draw disk name/mount point
        layout.set_text(&disk.name);
        cr.move_to(10.0, y);
        pangocairo::functions::layout_path(cr, layout);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.stroke_preserve().expect("Failed to stroke");
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().expect("Failed to fill");
        y += 20.0; // Space between name and bar
        
        // Draw progress bar (empty if loading, normal if ready)
        let percentage = if disk.is_loading { 0.0 } else { disk.used_percentage };
        draw_progress_bar(cr, 10.0, y, bar_width, bar_height, percentage);
        
        // Draw percentage if enabled
        if show_percentages {
            let percentage_text = if disk.is_loading {
                "Loading...".to_string()
            } else {
                format!("{:.1}%", disk.used_percentage)
            };
            layout.set_text(&percentage_text);
            cr.move_to(220.0, y);
            pangocairo::functions::layout_path(cr, layout);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke_preserve().expect("Failed to stroke");
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill().expect("Failed to fill");
        }
        
        y += 25.0; // Space after bar before next disk
    }
    
    y
}
