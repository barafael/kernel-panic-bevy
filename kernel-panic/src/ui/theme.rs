//! Shared visual constants for the in-game UI.
//!
//! Pull every color/spacing/font-size from here so the hue of a panel border
//! lives in exactly one place. Faction tints come from
//! [`crate::units::components::Faction::color`] — don't duplicate them.
//!
//! No persistence yet. When an options menu lands the static constants
//! become a `Theme` resource the panels read from.

use bevy::prelude::*;

/// Primary UI hue: the same vivid green as System units, slightly muted so
/// it doesn't read as harsh against neutral terrain. Borders, headings, and
/// Stop/Attack accents all derive from this.
pub const KP_GREEN: Color = Color::srgb(0.30, 0.95, 0.45);

/// Mid-green used for body text and inactive button labels.
pub const KP_GREEN_DIM: Color = Color::srgb(0.50, 0.85, 0.55);

/// Dark green-tinted black for panel backgrounds. Translucent so terrain
/// faintly reads through, like an HUD overlay rather than a solid box.
pub const PANEL_BG: Color = Color::srgba(0.02, 0.06, 0.03, 0.78);

/// Panel border / divider color. Slightly brighter than the body text so
/// the chrome stays distinct against terrain at any zoom level.
pub const PANEL_BORDER: Color = Color::srgba(0.20, 0.85, 0.35, 0.85);

/// Background color of an idle button (build icon, order button).
pub const BUTTON_BG: Color = Color::srgba(0.04, 0.10, 0.05, 0.85);

/// Background color of a hovered button.
#[allow(dead_code)]
pub const BUTTON_BG_HOVERED: Color = Color::srgba(0.08, 0.20, 0.10, 0.92);

/// Background color of a button being pressed.
pub const BUTTON_BG_PRESSED: Color = Color::srgba(0.15, 0.40, 0.20, 0.95);

/// Translucent black used as a backing for any small text that needs to
/// stay legible against varied terrain (queue badges, tooltip body, etc.).
pub const TEXT_BG: Color = Color::srgba(0.0, 0.0, 0.0, 0.6);

/// Health-bar / unit-info accent red for low HP (mirrors `health_color`).
#[allow(dead_code)]
pub const KP_RED: Color = Color::srgb(0.95, 0.30, 0.25);

/// Standard padding inside any panel (`Val::Px(PANEL_PADDING)`).
pub const PANEL_PADDING: f32 = 8.0;

/// Standard gap between sibling rows/columns inside a panel.
pub const PANEL_GAP: f32 = 6.0;

/// Title-line font size — used for panel headings and the unit name.
pub const TEXT_TITLE: f32 = 18.0;

/// Body-line font size — used for stat rows, button labels.
pub const TEXT_BODY: f32 = 14.0;

/// Small-text font size — queue badges, hotkey hints, footnotes.
pub const TEXT_SMALL: f32 = 12.0;

/// Build/order icon size. Used by build_menu and order_palette.
pub const ICON_SIZE: f32 = 56.0;

/// Width of the right-side build/order column.
pub const RIGHT_COLUMN_WIDTH: f32 = 220.0;

/// Width of the left-side info / minimap column.
pub const LEFT_COLUMN_WIDTH: f32 = 240.0;
