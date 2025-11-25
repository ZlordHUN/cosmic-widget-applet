// SPDX-License-Identifier: MPL-2.0

//! # COSMIC Theme Integration Module
//!
//! This module reads the COSMIC desktop environment theme settings to provide
//! accent colors and theme mode (dark/light) for the widget rendering.
//!
//! ## Theme Configuration Location
//!
//! COSMIC stores theme settings in `~/.config/cosmic/`:
//! - `com.system76.CosmicTheme.Mode/v1/is_dark` - Boolean for dark/light mode
//! - `com.system76.CosmicTheme.Dark/v1/accent` - Dark theme accent color
//! - `com.system76.CosmicTheme.Light/v1/accent` - Light theme accent color
//!
//! ## Color Format
//!
//! COSMIC stores colors in RON format with RGBA components (0.0-1.0).
//! We parse the `base` color from the accent configuration.
//!
//! ## Fallback Behavior
//!
//! If theme files cannot be read, sensible defaults are used:
//! - Dark mode: true (matches COSMIC default)
//! - Accent color: Blue (#6699FF / RGB 0.4, 0.6, 1.0)

use std::fs;
use std::path::PathBuf;

/// RGBA color with components in 0.0-1.0 range
#[derive(Debug, Clone, Copy)]
pub struct ThemeColor {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

impl Default for ThemeColor {
    fn default() -> Self {
        // Default to a pleasant blue accent
        Self {
            red: 0.4,
            green: 0.6,
            blue: 1.0,
            alpha: 1.0,
        }
    }
}

/// Theme information read from COSMIC configuration
#[derive(Debug, Clone)]
pub struct CosmicTheme {
    /// Whether dark mode is active
    pub is_dark: bool,
    /// Primary accent color
    pub accent: ThemeColor,
    /// Accent color with reduced opacity for backgrounds
    pub accent_bg: ThemeColor,
}

impl Default for CosmicTheme {
    fn default() -> Self {
        let accent = ThemeColor::default();
        Self {
            is_dark: true,
            accent,
            accent_bg: ThemeColor {
                alpha: 0.6,
                ..accent
            },
        }
    }
}

impl CosmicTheme {
    /// Read theme settings from COSMIC configuration files.
    ///
    /// Falls back to defaults if files cannot be read or parsed.
    pub fn load() -> Self {
        let mut theme = Self::default();
        
        // Get config directory
        let config_dir = match dirs::config_dir() {
            Some(dir) => dir.join("cosmic"),
            None => {
                log::warn!("Could not find config directory, using default theme");
                return theme;
            }
        };
        
        // Read dark/light mode
        theme.is_dark = Self::read_is_dark(&config_dir);
        
        // Read accent color based on current mode
        theme.accent = Self::read_accent_color(&config_dir, theme.is_dark);
        theme.accent_bg = ThemeColor {
            alpha: 0.6,
            ..theme.accent
        };
        
        log::info!(
            "Loaded COSMIC theme: is_dark={}, accent=({:.2}, {:.2}, {:.2})",
            theme.is_dark,
            theme.accent.red,
            theme.accent.green,
            theme.accent.blue
        );
        
        theme
    }
    
    /// Read the is_dark setting from theme mode config
    fn read_is_dark(config_dir: &PathBuf) -> bool {
        let mode_path = config_dir
            .join("com.system76.CosmicTheme.Mode")
            .join("v1")
            .join("is_dark");
        
        match fs::read_to_string(&mode_path) {
            Ok(content) => {
                let trimmed = content.trim();
                trimmed == "true"
            }
            Err(e) => {
                log::debug!("Could not read theme mode from {:?}: {}", mode_path, e);
                true // Default to dark mode
            }
        }
    }
    
    /// Read accent color from the appropriate theme config (dark or light)
    fn read_accent_color(config_dir: &PathBuf, is_dark: bool) -> ThemeColor {
        let theme_name = if is_dark {
            "com.system76.CosmicTheme.Dark"
        } else {
            "com.system76.CosmicTheme.Light"
        };
        
        let accent_path = config_dir
            .join(theme_name)
            .join("v1")
            .join("accent");
        
        match fs::read_to_string(&accent_path) {
            Ok(content) => Self::parse_accent_color(&content),
            Err(e) => {
                log::debug!("Could not read accent color from {:?}: {}", accent_path, e);
                ThemeColor::default()
            }
        }
    }
    
    /// Parse the RON-format accent color configuration.
    ///
    /// The format looks like:
    /// ```ron
    /// (
    ///     base: (
    ///         red: 0.41572583,
    ///         green: 0.35830325,
    ///         blue: 0.7028036,
    ///         alpha: 1.0,
    ///     ),
    ///     hover: (...),
    ///     ...
    /// )
    /// ```
    ///
    /// We extract the `base` color values using simple string parsing
    /// to avoid adding a RON dependency.
    fn parse_accent_color(content: &str) -> ThemeColor {
        let mut color = ThemeColor::default();
        
        // Find the "base:" section
        if let Some(base_start) = content.find("base:") {
            // Find the opening paren after "base:"
            if let Some(paren_start) = content[base_start..].find('(') {
                let base_section_start = base_start + paren_start;
                // Find the closing paren for the base section
                if let Some(paren_end) = content[base_section_start..].find(')') {
                    let base_section = &content[base_section_start..base_section_start + paren_end + 1];
                    
                    // Parse individual color components
                    if let Some(red) = Self::extract_float(base_section, "red:") {
                        color.red = red;
                    }
                    if let Some(green) = Self::extract_float(base_section, "green:") {
                        color.green = green;
                    }
                    if let Some(blue) = Self::extract_float(base_section, "blue:") {
                        color.blue = blue;
                    }
                    if let Some(alpha) = Self::extract_float(base_section, "alpha:") {
                        color.alpha = alpha;
                    }
                }
            }
        }
        
        color
    }
    
    /// Extract a float value following a key like "red:"
    fn extract_float(content: &str, key: &str) -> Option<f64> {
        content.find(key).and_then(|pos| {
            let start = pos + key.len();
            let remaining = &content[start..];
            
            // Skip whitespace
            let trimmed = remaining.trim_start();
            
            // Find the end of the number (comma or paren)
            let end = trimmed
                .find(|c: char| c == ',' || c == ')' || c == '\n')
                .unwrap_or(trimmed.len());
            
            let num_str = trimmed[..end].trim();
            num_str.parse::<f64>().ok()
        })
    }
    
    /// Get text color appropriate for the current theme mode.
    ///
    /// Returns white for dark mode, dark gray for light mode.
    pub fn text_color(&self) -> (f64, f64, f64) {
        if self.is_dark {
            (1.0, 1.0, 1.0)
        } else {
            (0.1, 0.1, 0.1)
        }
    }
    
    /// Get secondary/muted text color appropriate for the current theme mode.
    pub fn secondary_text_color(&self) -> (f64, f64, f64) {
        if self.is_dark {
            (0.7, 0.7, 0.7)
        } else {
            (0.4, 0.4, 0.4)
        }
    }
    
    /// Get background color for panels/cards appropriate for the current theme mode.
    pub fn panel_background(&self) -> (f64, f64, f64, f64) {
        if self.is_dark {
            (0.1, 0.1, 0.15, 0.7)
        } else {
            (0.95, 0.95, 0.97, 0.85)
        }
    }
    
    /// Get border color appropriate for the current theme mode.
    pub fn border_color(&self) -> (f64, f64, f64, f64) {
        if self.is_dark {
            (0.3, 0.3, 0.4, 0.9)
        } else {
            (0.7, 0.7, 0.75, 0.9)
        }
    }
    
    /// Get progress bar background color appropriate for the current theme mode.
    pub fn progress_background(&self) -> (f64, f64, f64, f64) {
        if self.is_dark {
            (0.3, 0.3, 0.3, 0.8)
        } else {
            (0.8, 0.8, 0.82, 0.9)
        }
    }
    
    /// Get the accent color as RGB tuple
    pub fn accent_rgb(&self) -> (f64, f64, f64) {
        (self.accent.red, self.accent.green, self.accent.blue)
    }
    
    /// Get the accent color as RGBA tuple
    pub fn accent_rgba(&self, alpha: f64) -> (f64, f64, f64, f64) {
        (self.accent.red, self.accent.green, self.accent.blue, alpha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_accent_color() {
        let content = r#"(
    base: (
        red: 0.41572583,
        green: 0.35830325,
        blue: 0.7028036,
        alpha: 1.0,
    ),
    hover: (
        red: 0.5,
        green: 0.5,
        blue: 0.5,
        alpha: 1.0,
    ),
)"#;
        
        let color = CosmicTheme::parse_accent_color(content);
        assert!((color.red - 0.41572583).abs() < 0.001);
        assert!((color.green - 0.35830325).abs() < 0.001);
        assert!((color.blue - 0.7028036).abs() < 0.001);
        assert!((color.alpha - 1.0).abs() < 0.001);
    }
    
    #[test]
    fn test_default_theme() {
        let theme = CosmicTheme::default();
        assert!(theme.is_dark);
        assert!((theme.accent.red - 0.4).abs() < 0.001);
    }
}
