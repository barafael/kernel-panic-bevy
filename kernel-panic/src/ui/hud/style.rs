//! Shared visual constants for the HUD.

use bevy::prelude::*;

pub const UI_BORDER_COLOR: Color = Color::linear_rgb(0.0, 0.7, 0.2);
pub const UI_BG_COLOR: Color = Color::srgba(0.0, 0.05, 0.0, 0.75);
pub const UI_TEXT_COLOR: Color = Color::linear_rgb(0.0, 1.0, 0.3);
pub const UI_TEXT_DIM: Color = Color::linear_rgb(0.0, 0.5, 0.15);

/// Tinted panel background used by build menus and the order palette:
/// a subtle green wash so HUD groups read as distinct surfaces against
/// the darker `UI_BG_COLOR` root panel.
pub const UI_PANEL_TINT: Color = Color::srgba(0.0, 0.1, 0.0, 0.6);

/// Dimmer slate used behind text rows that need contrast but shouldn't
/// compete with the green-tinted panels.
pub const UI_ROW_BG: Color = Color::srgba(0.1, 0.1, 0.1, 0.8);

/// Generic translucent black overlay used for ephemera like health-bar
/// backings.
pub const UI_OVERLAY_BLACK: Color = Color::srgba(0.0, 0.0, 0.0, 0.6);

pub const FONT_SIZE_TITLE: f32 = 18.0;
pub const FONT_SIZE_BODY: f32 = 14.0;
pub const FONT_SIZE_SMALL: f32 = 12.0;

pub const BUILD_ICON_SIZE: f32 = 64.0;
