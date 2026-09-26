//! Control-panel geometry, ported from Spring's `CGuiHandler::LoadConfig`
//! / `LayoutIcons` / `IconAtPos` with Kernel Panic's
//! `LuaUI/Widgets/KP_CtrlPanel.txt` values.
//!
//! Spring works in normalised screen coordinates: `x` in `[0, 1]` across
//! the view width, `y` in `[0, 1]` from the *bottom* of the view. The
//! icons are therefore `xIconSize` of the width wide and `yIconSize` of
//! the height tall — not square — and the whole panel scales with the
//! window. [`PanelConfig::icon_px`] converts to Bevy UI pixels (top-left
//! origin).

use bevy::prelude::*;

/// The subset of the Spring control-panel config KP sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PanelConfig {
    pub x_pos: f32,
    pub y_pos: f32,
    pub x_icons: usize,
    pub y_icons: usize,
    pub x_icon_size: f32,
    pub y_icon_size: f32,
    pub text_border: f32,
    pub icon_border: f32,
    pub frame_border: f32,
    /// `selectGaps 0`: the clickable area overlaps the gap between icons.
    pub select_gaps: bool,
}

/// `KP_CtrlPanel.txt`: a 3×9 grid at the left edge, from 14.7 % up the
/// screen, each icon 6 % of the width by 6 % of the height, no borders.
/// `prevPageSlot` / `nextPageSlot` are `auto` (the last two slots) and
/// `deadIconSlot auto` parses to none.
pub(crate) const KP_CTRL_PANEL: PanelConfig = PanelConfig {
    x_pos: 0.0,
    y_pos: 0.147,
    x_icons: 3,
    y_icons: 9,
    x_icon_size: 0.06,
    y_icon_size: 0.06,
    text_border: 0.0035,
    icon_border: 0.0,
    frame_border: 0.0,
    select_gaps: false,
};

/// Axis-aligned box in Spring's normalised GUI space (`y1` top, `y2`
/// bottom, as `IconInfo::visual` stores them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GuiBox {
    pub x1: f32,
    pub x2: f32,
    pub y1: f32,
    pub y2: f32,
}

/// What occupies one icon slot of a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SlotCmd {
    /// Index into the command list.
    Command(usize),
    Prev,
    Next,
}

impl PanelConfig {
    pub fn icons_per_page(&self) -> usize {
        self.x_icons * self.y_icons
    }

    fn x_step(&self) -> f32 {
        self.x_icon_size + 2.0 * self.icon_border
    }

    fn y_step(&self) -> f32 {
        self.y_icon_size + 2.0 * self.icon_border
    }

    pub fn prev_slot(&self) -> usize {
        self.icons_per_page() - 2
    }

    pub fn next_slot(&self) -> usize {
        self.icons_per_page() - 1
    }

    /// `buttonBox`: the frame around every icon.
    pub fn button_box(&self) -> GuiBox {
        GuiBox {
            x1: self.x_pos,
            x2: self.x_pos + 2.0 * self.frame_border + self.x_icons as f32 * self.x_step(),
            y1: self.y_pos + 2.0 * self.frame_border + self.y_icons as f32 * self.y_step(),
            y2: self.y_pos,
        }
    }

    /// `icon.visual` for page slot `slot` (row-major from the top-left).
    pub fn icon_box(&self, slot: usize) -> GuiBox {
        let bb = self.button_box();
        let fx = (slot % self.x_icons) as f32;
        let fy = (slot / self.x_icons) as f32;
        let full = self.frame_border + self.icon_border;
        let x1 = bb.x1 + full + fx * self.x_step();
        let y1 = bb.y1 - (full + fy * self.y_step());
        GuiBox {
            x1,
            x2: x1 + self.x_icon_size,
            y1,
            y2: y1 - self.y_icon_size,
        }
    }

    /// `icon.visual` of `slot` as a Bevy UI rect: (left, top, width,
    /// height) in logical pixels of a `view` sized window.
    pub fn icon_px(&self, slot: usize, view: Vec2) -> Rect {
        gui_to_px(self.icon_box(slot), view)
    }

    /// `CGuiHandler::IconAtPos`: the page slot under the cursor
    /// (`cursor` in Bevy logical pixels, top-left origin), or `None`
    /// outside the panel. Slots are clamped into the grid first, then
    /// tested against the (gap-bridging) selection box.
    pub fn slot_at(&self, cursor: Vec2, view: Vec2) -> Option<usize> {
        if view.x <= 0.0 || view.y <= 0.0 {
            return None;
        }
        let fx = cursor.x / view.x;
        let fy = 1.0 - cursor.y / view.y;
        let bb = self.button_box();
        if fx < bb.x1 || fx > bb.x2 || fy < bb.y2 || fy > bb.y1 {
            return None;
        }
        let xs = ((fx - (bb.x1 + self.frame_border)) / self.x_step()) as i32;
        let ys = (((bb.y1 - self.frame_border) - fy) / self.y_step()) as i32;
        let xs = xs.clamp(0, self.x_icons as i32 - 1) as usize;
        let ys = ys.clamp(0, self.y_icons as i32 - 1) as usize;
        let slot = ys * self.x_icons + xs;
        let vis = self.icon_box(slot);
        let no_gap = if self.select_gaps {
            0.0
        } else {
            self.icon_border + 0.0005
        };
        let inside = fx > vis.x1 - no_gap
            && fx < vis.x2 + no_gap
            && fy < vis.y1 + no_gap
            && fy > vis.y2 - no_gap;
        inside.then_some(slot)
    }

    /// `CGuiHandler::LayoutIcons`: distribute `cmd_count` commands over
    /// pages. The prev / next slots are always reserved (`auto` slots
    /// count as extra icons), so a page holds `icons_per_page - 2`
    /// commands; the arrows only appear when there is more than one page.
    pub fn layout(&self, cmd_count: usize) -> Vec<Vec<Option<SlotCmd>>> {
        let per_page = self.icons_per_page();
        let usable = per_page - 2;
        let page_count = cmd_count.div_ceil(usable).max(1);
        let multi = page_count > 1;
        let mut next_cmd = 0;
        (0..page_count)
            .map(|_| {
                (0..per_page)
                    .map(|slot| {
                        if slot == self.next_slot() {
                            multi.then_some(SlotCmd::Next)
                        } else if slot == self.prev_slot() {
                            multi.then_some(SlotCmd::Prev)
                        } else if next_cmd < cmd_count {
                            next_cmd += 1;
                            Some(SlotCmd::Command(next_cmd - 1))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .collect()
    }
}

/// Normalised GUI box → Bevy UI pixel rect (`min` = top-left).
pub(crate) fn gui_to_px(b: GuiBox, view: Vec2) -> Rect {
    Rect::new(
        b.x1 * view.x,
        (1.0 - b.y1) * view.y,
        b.x2 * view.x,
        (1.0 - b.y2) * view.y,
    )
}

/// Advance width of the UI font (Bevy's default FiraMono) per em.
pub(crate) const FONT_ADVANCE_EM: f32 = 0.6;
/// Glyph extent of a mixed-case line per em (cap height + descender),
/// Spring's `GetTextHeight` for a typical label.
const FONT_HEIGHT_EM: f32 = 0.9;

/// `CGuiHandler::DrawName`: the font size that makes `text` exactly fit
/// the icon minus `textBorder` on each side (and minus the LED strip on
/// state buttons) — Spring scales every label to fill its button.
pub(crate) fn fit_font_size(text: &str, icon: Vec2, text_border: Vec2, led_shrink: f32) -> f32 {
    let chars = text.chars().count().max(1) as f32;
    let avail_w = (icon.x - 2.0 * text_border.x).max(1.0);
    let avail_h = (icon.y - 2.0 * text_border.y - led_shrink).max(1.0);
    (avail_w / (chars * FONT_ADVANCE_EM)).min(avail_h / FONT_HEIGHT_EM)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: Vec2 = Vec2::new(1920.0, 1080.0);

    /// KP's panel on a 1080p screen: 115.2 × 64.8 px icons from the left
    /// edge, top row at 31.3 % of the height, bottom at 85.3 %.
    #[test]
    fn kp_geometry_matches_guihandler() {
        let cfg = KP_CTRL_PANEL;
        let bb = cfg.button_box();
        assert!((bb.x2 - 0.18).abs() < 1e-6);
        assert!((bb.y1 - 0.687).abs() < 1e-6);
        let first = cfg.icon_px(0, VIEW);
        assert!((first.min.x - 0.0).abs() < 1e-3);
        assert!((first.width() - 115.2).abs() < 1e-3);
        assert!((first.height() - 64.8).abs() < 1e-3);
        assert!((first.min.y - 0.313 * 1080.0).abs() < 1e-2);
        let last = cfg.icon_px(26, VIEW);
        assert!((last.min.x - 230.4).abs() < 1e-3);
        assert!((last.max.y - (1.0 - 0.147) * 1080.0).abs() < 1e-2);
    }

    /// IconAtPos round-trips slot centres and rejects points off the panel.
    #[test]
    fn slot_at_finds_icons() {
        let cfg = KP_CTRL_PANEL;
        for slot in 0..cfg.icons_per_page() {
            let r = cfg.icon_px(slot, VIEW);
            assert_eq!(cfg.slot_at(r.center(), VIEW), Some(slot));
        }
        assert_eq!(cfg.slot_at(Vec2::new(400.0, 500.0), VIEW), None);
        assert_eq!(cfg.slot_at(Vec2::new(50.0, 100.0), VIEW), None);
        assert_eq!(cfg.slot_at(Vec2::new(50.0, 1000.0), VIEW), None);
    }

    /// A page holds 25 commands (the last two slots are the page-arrow
    /// slots, empty on a single page); a 26th command needs paging.
    #[test]
    fn paging_reserves_arrow_slots() {
        let cfg = KP_CTRL_PANEL;
        let one = cfg.layout(25);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0][24], Some(SlotCmd::Command(24)));
        assert_eq!(one[0][25], None);
        assert_eq!(one[0][26], None);
        let two = cfg.layout(28);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0][24], Some(SlotCmd::Command(24)));
        assert_eq!(two[0][25], Some(SlotCmd::Prev));
        assert_eq!(two[0][26], Some(SlotCmd::Next));
        assert_eq!(two[1][0], Some(SlotCmd::Command(25)));
        assert_eq!(two[1][2], Some(SlotCmd::Command(27)));
        assert_eq!(two[1][3], None);
        assert_eq!(two[1][26], Some(SlotCmd::Next));
    }

    /// Labels scale to fill the icon: short names are height-bound,
    /// long ones width-bound.
    #[test]
    fn labels_fill_the_button() {
        let icon = Vec2::new(115.2, 64.8);
        let tb = Vec2::new(0.0035 * 1920.0, 0.0035 * 1080.0);
        let stop = fit_font_size("Stop", icon, tb, 0.0);
        assert!((stop * 4.0 * FONT_ADVANCE_EM - (115.2 - 2.0 * tb.x)).abs() < 1e-3);
        let s = fit_font_size("S", icon, tb, 0.0);
        assert!((s * FONT_HEIGHT_EM - (64.8 - 2.0 * tb.y)).abs() < 1e-3);
    }
}
