//! The game-over panel's Statistics page (`stats`): one chart of the
//! picked stat over the match, a line per seat, the stat list on the
//! left and a legend row above the chart whose plates hide or show a
//! seat. Ticks on a line mark a landmark finished (pointing up) or lost
//! (pointing down) at that moment. After Beyond All Reason's team stats
//! graphs (`luaui/Include/teamstats_graphs.lua`), kept to four stats.
//!
//! The chart is drawn into an image on the CPU, once per redraw (a pick,
//! a toggle, a new sample while the page is up), and shown as an
//! `ImageNode`; the axis labels are text nodes around it.

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::ui::UiTransform;

use bevy::math::ops;

use super::{Anchor, ButtonSpec, LabelSpec, MenuAction, NAV_BLUE, anchored};
use crate::sim::DetFloat;
use crate::stats::{MatchStats, SeatStats, Stat};

/// A seat's colour: its units' in the match
/// ([`TeamColors`](crate::units::team_colors::TeamColors)), so a line
/// reads as the army it counts, and two seats of one faction differ.
fn seat_rgb(stats: &MatchStats, seat: usize) -> [u8; 3] {
    use crate::units::components::Faction;
    let factions = stats
        .seats
        .iter()
        .map(|s| s.faction.unwrap_or(Faction::System));
    let colors = crate::units::team_colors::TeamColors::from_factions(factions);
    let c = Srgba::from(colors.of(seat as u8, Faction::System).color());
    [c.red, c.green, c.blue].map(|v| (v.det_clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn seat_color(stats: &MatchStats, seat: usize) -> Color {
    let [r, g, b] = seat_rgb(stats, seat);
    Color::srgb_u8(r, g, b)
}

/// The chart area, in window fractions (`x` from the left, `y` from the
/// bottom, as [`anchored`]).
const CHART_LEFT: f32 = 0.2;
const CHART_RIGHT: f32 = 0.97;
const CHART_BOTTOM: f32 = 0.18;
const CHART_TOP: f32 = 0.8;

/// Behind the page: darker than the panel's glass, so the match moving
/// behind it doesn't read as part of the chart.
const STATS_GLASS: Color = Color::srgba(0.0, 0.0, 0.0, 0.88);

/// What the page shows. Part of the game-over panel's redraw key.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StatsView {
    pub(crate) open: bool,
    pub(crate) stat: Stat,
    /// Seats hidden from the chart, one bit each.
    pub(crate) hidden: u64,
}

impl Default for StatsView {
    fn default() -> Self {
        Self {
            open: false,
            stat: Stat::ArmyValue,
            hidden: 0,
        }
    }
}

/// What the page's buttons do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatsAction {
    Open(bool),
    Pick(Stat),
    /// Hide or show seat `n`.
    Toggle(u8),
}

impl StatsView {
    pub(super) fn apply(&mut self, action: StatsAction) {
        match action {
            StatsAction::Open(open) => self.open = open,
            StatsAction::Pick(stat) => self.stat = stat,
            StatsAction::Toggle(seat) => self.hidden ^= 1u64 << (seat % 64),
        }
    }

    fn shown(&self, seat: usize) -> bool {
        self.hidden & (1u64 << (seat % 64)) == 0
    }
}

/// A seat's legend label: "P1 System (you)", "AI 2 Hacker".
fn seat_label(i: usize, seat: &SeatStats, local: Option<u8>) -> String {
    let who = if seat.ai { "AI" } else { "P" };
    let faction = seat.faction.map(|f| format!(" {f:?}")).unwrap_or_default();
    let you = if local == Some(i as u8) { " (you)" } else { "" };
    format!("{who}{}{faction}{you}", i + 1)
}

/// The y axis: a round step and how many of them reach past `max`.
fn y_axis(max: f32) -> (f32, u32) {
    let max = max.det_max(1.0);
    let rough = max / 4.0;
    let magnitude = ops::powf(10.0, ops::log10(rough).floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * magnitude)
        .find(|&s| s >= rough)
        .unwrap_or(10.0 * magnitude)
        .det_max(1.0);
    (step, (max / step).ceil().det_max(1.0) as u32)
}

/// An axis value: `950`, `12k`, `2.5k`, `1.2M`.
fn format_value(v: f32) -> String {
    let short = |x: f32, unit: &str| {
        if (x - x.round()).abs() < 0.05 {
            format!("{}{unit}", x.round())
        } else {
            format!("{x:.1}{unit}")
        }
    };
    if v >= 1e6 {
        short(v / 1e6, "M")
    } else if v >= 1e3 {
        short(v / 1e3, "k")
    } else {
        short(v, "")
    }
}

/// `m:ss`.
fn format_time(seconds: f32) -> String {
    let s = seconds.det_max(0.0) as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

/// `series` at `tick`, between the samples around it.
fn value_at(ticks: &[u32], series: &[f32], tick: u32) -> f32 {
    let i = ticks.partition_point(|&t| t <= tick);
    match (i.checked_sub(1), ticks.get(i)) {
        (None, _) => series.first().copied().unwrap_or(0.0),
        (Some(a), None) => series.get(a).copied().unwrap_or(0.0),
        (Some(a), Some(&tb)) => {
            let ta = ticks[a];
            let f = (tick - ta) as f32 / (tb - ta).max(1) as f32;
            let (va, vb) = (series[a], series.get(a + 1).copied().unwrap_or(series[a]));
            va + (vb - va) * f
        }
    }
}

/// An RGBA8 (sRGB) canvas.
struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32, fill: [u8; 4]) -> Self {
        Self {
            w,
            h,
            px: fill.repeat((w * h) as usize),
        }
    }

    fn set(&mut self, x: i32, y: i32, c: [u8; 4]) {
        if x >= 0 && y >= 0 && (x as u32) < self.w && (y as u32) < self.h {
            let i = (y as usize * self.w as usize + x as usize) * 4;
            self.px[i..i + 4].copy_from_slice(&c);
        }
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: [u8; 4]) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.set(x, y, c);
            }
        }
    }

    /// A line `brush` pixels wide.
    fn line(&mut self, a: Vec2, b: Vec2, brush: i32, c: [u8; 4]) {
        let steps = (b - a).abs().max_element().ceil().det_max(1.0) as i32;
        let lo = -(brush - 1) / 2;
        let hi = brush / 2;
        for s in 0..=steps {
            let p = a.lerp(b, s as f32 / steps as f32);
            let (x, y) = (p.x.round() as i32, p.y.round() as i32);
            self.rect(x + lo, y + lo, x + hi, y + hi, c);
        }
    }

    /// A filled triangle `size` high with its point at `(x, y)`,
    /// pointing up (`up`) or down, its base away from the point.
    fn marker(&mut self, x: i32, y: i32, size: i32, up: bool, fill: [u8; 4], edge: [u8; 4]) {
        for row in 0..=size {
            let half = row / 2 + 1;
            let yy = if up { y + row } else { y - row };
            for dx in -half..=half {
                let rim = row == size || dx.abs() == half;
                self.set(x + dx, yy, if rim { edge } else { fill });
            }
        }
    }
}

/// The chart image's pixels: grid, a line per shown seat, the markers.
/// `steps` grid lines over `ymax`.
fn draw_chart(
    stats: &MatchStats,
    view: &StatsView,
    (w, h): (u32, u32),
    ymax: f32,
    steps: u32,
    scale: f32,
) -> Canvas {
    let mut canvas = Canvas::new(w, h, [0, 14, 0, 240]);
    let grid = [0, 90, 0, 255];
    let (wf, hf) = ((w - 1) as f32, (h - 1) as f32);
    for k in 0..=steps {
        let y = (hf - hf * k as f32 / steps as f32).round() as i32;
        canvas.rect(0, y, w as i32 - 1, y, grid);
    }
    for k in 0..=4 {
        let x = (wf * k as f32 / 4.0).round() as i32;
        canvas.rect(x, 0, x, h as i32 - 1, grid);
    }
    let (Some(&t0), Some(&t1)) = (stats.ticks.first(), stats.ticks.last()) else {
        return canvas;
    };
    let span = (t1 - t0).max(1) as f32;
    let at = |tick: u32, v: f32| {
        Vec2::new(
            wf * (tick.saturating_sub(t0)) as f32 / span,
            hf - hf * (v / ymax).det_clamp(0.0, 1.0),
        )
    };
    let brush = (2.0 * scale).round().det_max(2.0) as i32;
    let alpha = |[r, g, b]: [u8; 3]| [r, g, b, 255];
    for (i, seat) in stats.seats.iter().enumerate() {
        if !view.shown(i) {
            continue;
        }
        let series = seat.series(view.stat);
        let color = alpha(seat_rgb(stats, i));
        let mut points = stats.ticks.iter().zip(series).map(|(&t, &v)| at(t, v));
        let Some(mut prev) = points.next() else {
            continue;
        };
        for p in points {
            canvas.line(prev, p, brush, color);
            prev = p;
        }
    }
    let size = (12.0 * scale).round().det_max(10.0) as i32;
    for event in &stats.events {
        let i = event.owner as usize;
        let Some(seat) = stats.seats.get(i).filter(|_| view.shown(i)) else {
            continue;
        };
        let p = at(
            event.tick,
            value_at(&stats.ticks, seat.series(view.stat), event.tick),
        );
        let (x, y) = (p.x.round() as i32, p.y.round() as i32);
        // Built: an arrowhead under the line pointing up at it; lost:
        // one above pointing down.
        let (tip, up) = if event.built {
            (y + brush, true)
        } else {
            (y - brush, false)
        };
        canvas.marker(x, tip, size, up, alpha(seat_rgb(stats, i)), [0, 0, 0, 255]);
    }
    canvas
}

/// A text label at window fractions.
fn label(
    commands: &mut Commands,
    root: Entity,
    text: String,
    size: f32,
    pos: (f32, f32),
    anchor: Anchor,
) {
    LabelSpec::new(&text, size, pos, anchor).spawn(commands, root);
}

/// The page, into the game-over panel's `root`.
pub(super) fn stats_page(
    commands: &mut Commands,
    root: Entity,
    window: &Window,
    images: &mut Assets<Image>,
    stats: &MatchStats,
    view: &StatsView,
    local: Option<u8>,
) {
    commands.entity(root).insert(BackgroundColor(STATS_GLASS));
    let size = window.height() / 32.0;
    LabelSpec::new(
        "Statistics",
        window.height() / 20.0,
        (0.5, 0.985),
        Anchor::Ct,
    )
    .frame(NAV_BLUE)
    .spawn(commands, root);
    // The stat list.
    for (k, stat) in Stat::ALL.into_iter().enumerate() {
        ButtonSpec::new(
            stat.label(),
            NAV_BLUE,
            size * 0.85,
            (0.02, CHART_TOP - 0.03 - 0.075 * k as f32),
            Anchor::Lt,
            MenuAction::Stats(StatsAction::Pick(stat)),
        )
        .min_width(14.0)
        .chosen(view.stat == stat)
        .spawn(commands, root);
    }
    ButtonSpec::new(
        "Back",
        NAV_BLUE,
        size,
        (0.02, 0.06),
        Anchor::Lc,
        MenuAction::Stats(StatsAction::Open(false)),
    )
    .min_width(14.0)
    .spawn(commands, root);

    // The legend: a plate per seat, dim while hidden.
    let (mut legend, transform) = anchored(CHART_LEFT, 0.855, Anchor::Lc);
    legend.column_gap = Val::Px(size * 0.5);
    legend.flex_wrap = FlexWrap::Wrap;
    legend.max_width = Val::Percent((CHART_RIGHT - CHART_LEFT) * 100.0);
    let row = commands.spawn((legend, transform)).id();
    commands.entity(root).add_child(row);
    for (i, seat) in stats.seats.iter().enumerate() {
        let color = if view.shown(i) {
            seat_color(stats, i)
        } else {
            seat_color(stats, i).darker(0.5)
        };
        let text = seat_label(i, seat, local);
        ButtonSpec::cell(
            &text,
            color,
            size * 0.8,
            MenuAction::Stats(StatsAction::Toggle(i as u8)),
        )
        .chosen(view.shown(i))
        .spawn(commands, row);
    }

    if stats.ticks.is_empty() {
        label(
            commands,
            root,
            "No statistics yet".into(),
            size,
            (0.5, 0.5),
            Anchor::Cc,
        );
        return;
    }

    let max = stats
        .seats
        .iter()
        .enumerate()
        .filter(|(i, _)| view.shown(*i))
        .flat_map(|(_, s)| s.series(view.stat).iter().copied())
        .fold(0.0f32, |a, b| a.det_max(b));
    let (step, steps) = y_axis(max);
    let ymax = step * steps as f32;
    let scale = window.scale_factor();
    let px = (
        ((CHART_RIGHT - CHART_LEFT) * window.physical_width() as f32).det_max(2.0) as u32,
        ((CHART_TOP - CHART_BOTTOM) * window.physical_height() as f32).det_max(2.0) as u32,
    );
    let canvas = draw_chart(stats, view, px, ymax, steps, scale);
    let image = images.add(Image::new(
        Extent3d {
            width: canvas.w,
            height: canvas.h,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        canvas.px,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    ));
    let chart = commands
        .spawn((
            super::MenuFrame,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(CHART_LEFT * 100.0),
                top: Val::Percent((1.0 - CHART_TOP) * 100.0),
                width: Val::Percent((CHART_RIGHT - CHART_LEFT) * 100.0),
                height: Val::Percent((CHART_TOP - CHART_BOTTOM) * 100.0),
                ..default()
            },
            UiTransform::default(),
            ImageNode::new(image),
            bevy::picking::Pickable::IGNORE,
        ))
        .id();
    commands.entity(root).add_child(chart);

    let small = size * 0.75;
    for k in 0..=steps {
        let y = CHART_BOTTOM + (CHART_TOP - CHART_BOTTOM) * k as f32 / steps as f32;
        label(
            commands,
            root,
            format_value(step * k as f32),
            small,
            (CHART_LEFT - 0.005, y),
            Anchor::Rc,
        );
    }
    let (t0, t1) = (stats.seconds(0), stats.seconds(stats.ticks.len() - 1));
    for k in 0..=4 {
        let f = k as f32 / 4.0;
        let anchor = match k {
            0 => Anchor::Lt,
            4 => Anchor::Rt,
            _ => Anchor::Ct,
        };
        label(
            commands,
            root,
            format_time(t0 + (t1 - t0) * f),
            small,
            (
                CHART_LEFT + (CHART_RIGHT - CHART_LEFT) * f,
                CHART_BOTTOM - 0.01,
            ),
            anchor,
        );
    }
    let built = stats.events.iter().filter(|e| e.built).count();
    let lost = stats.events.len() - built;
    LabelSpec::new(
        &format!(
            "{}\nArrowheads: a homebase, minifac or special finished ({built}, up) or lost ({lost}, down)",
            view.stat.explain()
        ),
        small * 0.9,
        (0.5 * (CHART_LEFT + CHART_RIGHT), 0.075),
        Anchor::Cc,
    )
    .spawn(commands, root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::BuildingEvent;
    use crate::units::content::definitions::UnitKind;

    #[test]
    fn axis_steps_are_round() {
        assert_eq!(y_axis(0.0), (1.0, 1));
        assert_eq!(y_axis(7.0), (2.0, 4));
        assert_eq!(y_axis(95.0), (25.0, 4));
        assert_eq!(y_axis(1234.0), (500.0, 3));
        assert_eq!(y_axis(40_000.0), (10_000.0, 4));
    }

    #[test]
    fn values_and_times_read_short() {
        assert_eq!(format_value(0.0), "0");
        assert_eq!(format_value(250.0), "250");
        assert_eq!(format_value(2500.0), "2.5k");
        assert_eq!(format_value(12_000.0), "12k");
        assert_eq!(format_value(1_500_000.0), "1.5M");
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(754.0), "12:34");
    }

    #[test]
    fn events_sit_on_the_line_between_samples() {
        let ticks = [0, 30, 60];
        let series = [0.0, 10.0, 30.0];
        assert_eq!(value_at(&ticks, &series, 15), 5.0);
        assert_eq!(value_at(&ticks, &series, 45), 20.0);
        assert_eq!(value_at(&ticks, &series, 90), 30.0);
    }

    fn two_seats() -> MatchStats {
        let mut stats = MatchStats::default();
        stats.ticks = vec![0, 30, 60];
        for v in [[0.0, 50.0, 100.0], [100.0, 50.0, 0.0]] {
            let mut seat = SeatStats::default();
            // A mirror: the two lines still differ.
            seat.faction = Some(crate::units::components::Faction::Network);
            seat.units = v.to_vec();
            stats.seats.push(seat);
        }
        stats.events.push(BuildingEvent {
            tick: 30,
            owner: 0,
            kind: UnitKind::Socket,
            built: true,
        });
        stats
    }

    #[test]
    fn the_chart_draws_the_shown_seats() {
        let stats = two_seats();
        let mut view = StatsView {
            stat: Stat::Units,
            ..default()
        };
        let pixel = |c: &Canvas, x: u32, y: u32| {
            let i = ((y * c.w + x) * 4) as usize;
            [c.px[i], c.px[i + 1], c.px[i + 2]]
        };
        let c = draw_chart(&stats, &view, (101, 101), 100.0, 4, 1.0);
        // Seat 0 rises from the bottom-left, seat 1 falls from the top-left.
        assert_eq!(pixel(&c, 1, 99), seat_rgb(&stats, 0));
        assert_eq!(pixel(&c, 1, 1), seat_rgb(&stats, 1));
        // The Socket's arrowhead under seat 0's line mid-chart.
        assert_eq!(pixel(&c, 50, 50 + 2 + 3), seat_rgb(&stats, 0));
        view.apply(StatsAction::Toggle(1));
        let c = draw_chart(&stats, &view, (101, 101), 100.0, 4, 1.0);
        assert_ne!(pixel(&c, 1, 1), seat_rgb(&stats, 1), "seat 1 hidden");
    }
}
