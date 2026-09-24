// SPDX-License-Identifier: MPL-2.0

//! Shared applications and monitoring services for COSMIC Widget.
//!
//! The entry points in `src/bin` select an application. Monitoring services,
//! configuration, and runtime helpers are shared by the production overlay
//! and the retained legacy renderer.

pub mod applet;
pub mod legacy;
pub mod overlay;
pub mod settings;

mod config;
#[doc(hidden)]
pub mod i18n;
mod monitors;
mod runtime;
