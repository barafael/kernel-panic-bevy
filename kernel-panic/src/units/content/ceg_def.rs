//! The CEG definition tree, in source.
//!
//! Every [`CegKind`] has one [`ExplosionDef`] behind it, written as a
//! `static` in [`super::ceg_defs`] and reached through [`CegKind::def`]:
//! total, load-free, and typed all the way down. The registry, the
//! `Option`, the `bool` and the start-up warning the TDF path needed to
//! survive a section that does not exist go with it
//! (`docs/ceg-in-source-plan.md` §0) — not in this commit: the runtime
//! still evaluates the TDFs, which is what the `#[allow(dead_code)]` on
//! this module in `mod.rs` is for.
//!
//! ## Where the data comes from
//!
//! The 50 statics are *generated* from `assets/defs/explosions/*.tdf`
//! — see the ignored `gen_ceg_defs` test below — and
//! checked against them by
//! [`cegs::tests::every_source_def_matches_the_parsed_tdf`](super::cegs::tests).
//! One is how the text got written; the other is proof it says what the
//! TDF says. Hand-edit a static and that test fails.
//!
//! ## What the source form drops
//!
//! Four things the parsed tree carries that a `static` does not need,
//! and four leniencies of the parsed tree it refuses to copy — the four
//! are the §3 decisions of the plan:
//!
//! | dropped | because |
//! |---|---|
//! | `EffectLayer::class` | the [`EffectProperties`] variant *is* the class |
//! | `ExplosionDef::id` | the [`CegKind`] the def is reached through *is* the name |
//! | `EffectClass::{Stars, Other}`, `EffectProperties::Raw` | no shipped CEG uses them (§1) |
//! | `CegOp::Other` | nothing has to round-trip an opcode the source cannot say |
//!
//! | instead of | the source says |
//! |---|---|
//! | `texture: "none"` / `""` | `texture: None`, and the layer still exists (§3.3) |
//! | `explosion_generator: "custom:x"` | `target: CegKind::X`, resolved at compile time |
//! | `CegOp::Other('y' \| 'x', n)` | [`CegOp::Yank`] / [`CegOp::MulBuf`], which *evaluate* (§3.2) |
//! | a `[groundflash]` with `ttl = 0` | `ground_flash: None`, which is what is drawn (§3.4) |

use crate::sim::DetFloat;
use strum::VariantArray;

use super::cegs::CegKind;

// ─── The tree ───────────────────────────────────────────────────────

/// One `[name]` section of an upstream `explosions/*.tdf`: the effect
/// layers, plus the ground flash if there is one the engine draws.
#[derive(Debug, PartialEq)]
pub struct ExplosionDef {
    /// The `[burst]` / `[circle]` / `[tracers]` subsections, in the
    /// order the file lists them.
    pub effects: &'static [EffectLayer],
    /// `None` covers both "no `[groundflash]` block" and "one with
    /// `ttl = 0`", which the engine does not draw either
    /// (`flashAgeSpeed = 0`, §3.4).
    pub ground_flash: Option<GroundFlash>,
}

/// One visual effect layer within an explosion.
#[derive(Debug, PartialEq)]
pub struct EffectLayer {
    /// The subsection name as authored (`"burst"`, `"squarecloud"`).
    /// Presentation: it is what a reviewer matches against the TDF.
    pub name: &'static str,
    /// Whether the layer fires for an air / ground / water impact.
    pub air: bool,
    pub ground: bool,
    pub water: bool,
    /// How many times the layer fires. `0` disables it — upstream
    /// `psi.count = max(0, GetInt("count", 1))`, and
    /// `network_flowtrail`'s `Circle` layer authors exactly that.
    pub count: u32,
    /// The class, as the variant.
    pub properties: EffectProperties,
}

/// The four effect classes the shipped CEGs use. The class string is
/// gone: `CSimpleParticleSystem` is [`EffectProperties::Particle`],
/// `CBitmapMuzzleFlame` is [`EffectProperties::Flame`],
/// `CExpGenSpawner` is [`EffectProperties::Spawner`] and `explspike` is
/// [`EffectProperties::Spike`].
#[derive(Debug, PartialEq)]
pub enum EffectProperties {
    Particle(ParticleProperties),
    Flame(FlameProperties),
    Spawner(SpawnerProperties),
    Spike(SpikeProperties),
}

/// `CSimpleParticleSystem`: a burst of physics particles.
#[derive(Debug, PartialEq)]
pub struct ParticleProperties {
    /// `None` is `Texture=none` — a layer that spawns particles nobody
    /// can see, kept because upstream ships it (§3.3).
    pub texture: Option<CegTexture>,
    pub color_map: ColorMap,
    pub num_particles: CegExpr,
    pub particle_life: CegExpr,
    pub particle_life_spread: CegExpr,
    pub particle_speed: CegExpr,
    pub particle_speed_spread: CegExpr,
    pub particle_size: CegExpr,
    pub particle_size_spread: CegExpr,
    pub size_growth: CegExpr,
    /// Per-frame size multiplier; the TDF default is `1` ("no change").
    pub size_mod: CegExpr,
    pub emit_rot: CegExpr,
    pub emit_rot_spread: CegExpr,
    /// Per-frame velocity multiplier; the TDF default is `1`.
    pub airdrag: CegExpr,
    pub gravity: CegVec3,
    pub pos: CegVec3,
    pub emit_vector: EmitVector,
    pub directional: bool,
    pub always_visible: bool,
}

/// `CBitmapMuzzleFlame`: a textured beam, shockwave or flame billboard.
#[derive(Debug, PartialEq)]
pub struct FlameProperties {
    pub side_texture: Option<CegTexture>,
    pub front_texture: Option<CegTexture>,
    pub color_map: ColorMap,
    pub size: CegExpr,
    pub length: CegExpr,
    pub size_growth: CegExpr,
    pub ttl: CegExpr,
    pub front_offset: CegExpr,
    pub speed: CegExpr,
    pub dir: CegVec3,
    pub pos: CegVec3,
    pub always_visible: bool,
}

/// `CExpGenSpawner`: replays another CEG after a delay.
#[derive(Debug, PartialEq)]
pub struct SpawnerProperties {
    /// The chained CEG. The TDF spells this `explosion_generator` as a
    /// `custom:NAME` string that `spawn_delayed` looked up per spawn and
    /// silently dropped on a miss; here it is a variant.
    pub target: CegKind,
    pub delay: CegExpr,
    /// The child's damage: what the child's own `d` ops see. That is
    /// what `damage=0 i1` in the data means — one damage per index.
    pub damage: CegExpr,
    pub dir: CegVec3,
    pub pos: CegVec3,
    pub speed: CegVec3,
    pub always_visible: bool,
}

/// `explspike`: a camera-facing streak that grows and fades.
#[derive(Debug, PartialEq)]
pub struct SpikeProperties {
    pub length: CegExpr,
    pub width: CegExpr,
    pub alpha: CegExpr,
    pub alpha_decay: CegExpr,
    /// Unnormalised direction; its length also sets the per-frame
    /// length growth.
    pub dir: CegVec3,
    pub pos: CegVec3,
    /// Tint; upstream default `(1, 0.8, 0.5)`.
    pub color: Option<CegVec3>,
}

/// The `[groundflash]` block: a ring and a filled circle on the terrain.
/// The four fields' upstream comments (`// flash size`,
/// `// initial alpha of the flash`, `// size increase/frame`,
/// `// initial alpha of the ring`) describe the field rather than the
/// value, so they live here once instead of in all twelve blocks.
#[derive(Debug, PartialEq)]
pub struct GroundFlash {
    pub flash_size: f32,
    pub flash_alpha: f32,
    pub circle_growth: f32,
    pub circle_alpha: f32,
    /// Frames the flash lives. Always `> 0` here: a `0` ttl means the
    /// engine draws nothing, and such a block is `ground_flash: None`.
    pub ttl: f32,
    pub color: [f32; 3],
}

// ─── Values ─────────────────────────────────────────────────────────

/// A CEG property value: the folded constant almost every property is,
/// or the handful of programs the data actually writes.
///
/// `Lit` is what the parser's `CegExpr::literal()` computes at runtime
/// for ~1900 of the 2060 TDF lines; making it a variant means the spawn
/// path never re-derives it and a reader sees at a glance which
/// properties are worth reading closely.
#[derive(Debug, PartialEq)]
pub enum CegExpr {
    Lit(f32),
    Ops(&'static [CegOp]),
}

/// One operation of a [`CegExpr`]. The set is closed: it is the four
/// opcodes the parser models plus the two it used to skip (§3.2), which
/// is everything the shipped data uses. The remaining upstream opcodes
/// (`a` add-buffer, `p`/`q` pow, `k` sawtooth, `s` discrete, `m` sine)
/// are not in the data and are not modelled; a def that wants one adds
/// the variant.
#[derive(Debug, PartialEq)]
pub enum CegOp {
    /// Literal add.
    Add(f32),
    /// Uniform random in `[0, v)`.
    Rand(f32),
    /// Per-spawn index × `v` — steps by `v` each particle of the burst.
    Index(f32),
    /// Exploding weapon's damage × `v`.
    Damage(f32),
    /// `y<n>`: store the accumulator in buffer *n* and reset it to zero.
    Yank(u8),
    /// `x<n>`: multiply the accumulator by buffer *n*.
    MulBuf(u8),
}

/// What [`CegOp`] evaluates against: which particle of the burst this
/// is, how hard the explosion was, and one roll of the dice.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EvalCtx {
    /// 0-based position in the burst; drives [`CegOp::Index`].
    pub index: u32,
    /// Weapon damage; drives [`CegOp::Damage`].
    pub damage: f32,
    /// Uniform pseudo-random in `[0, 1)`; drives [`CegOp::Rand`].
    pub rand01: f32,
}

/// Three independently-scaled axes.
#[derive(Debug, PartialEq)]
pub struct CegVec3 {
    pub x: CegExpr,
    pub y: CegExpr,
    pub z: CegExpr,
}

/// `emitVector`: the burst's emission axis.
#[derive(Debug, PartialEq)]
pub enum EmitVector {
    /// The firing direction at spawn time (`emitVector=dir`).
    Direction,
    /// A literal axis, which may still carry spread.
    Literal(CegVec3),
}

/// `colorMap`: evenly-spaced RGBA stops, interpolated over the
/// particle's life. Rows rather than the TDF's flat run of floats.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorMap(pub &'static [[f32; 4]]);

impl ColorMap {
    /// Linearly interpolate at `t`, the stops evenly spaced. Empty is
    /// opaque white, one stop is that stop, and outside `[0, 1]` it
    /// clamps rather than extrapolating — the same arithmetic the parsed
    /// map ran, so a palette colours exactly what it used to.
    pub fn sample(&self, t: f32) -> [f32; 4] {
        match self.0.len() {
            0 => [1.0, 1.0, 1.0, 1.0],
            1 => self.0[0],
            n => {
                let t = t.det_clamp(0.0, 1.0);
                let segs = n - 1;
                let scaled = t * segs as f32;
                let idx = (scaled as usize).min(segs - 1);
                let local = scaled - idx as f32;
                let (a, b) = (self.0[idx], self.0[idx + 1]);
                [
                    a[0] + (b[0] - a[0]) * local,
                    a[1] + (b[1] - a[1]) * local,
                    a[2] + (b[2] - a[2]) * local,
                    a[3] + (b[3] - a[3]) * local,
                ]
            }
        }
    }
}

/// Every axis zero — the most common vector in the data by a distance.
pub const ZERO: CegVec3 = CegVec3 {
    x: CegExpr::Lit(0.0),
    y: CegExpr::Lit(0.0),
    z: CegExpr::Lit(0.0),
};

/// A constant [`CegVec3`], for the properties with no spread on any axis.
pub const fn vec3(x: f32, y: f32, z: f32) -> CegVec3 {
    CegVec3 {
        x: CegExpr::Lit(x),
        y: CegExpr::Lit(y),
        z: CegExpr::Lit(z),
    }
}

impl CegExpr {
    /// The value this always evaluates to, if it always evaluates to the
    /// same value. The spawn path uses it to skip the roll it cannot see.
    pub const fn literal(&self) -> Option<f32> {
        match self {
            Self::Lit(v) => Some(*v),
            Self::Ops(_) => None,
        }
    }
}

impl CegVec3 {
    /// Evaluate all three axes against the same context.
    pub fn eval(&self, ctx: &EvalCtx) -> [f32; 3] {
        [self.x.eval(ctx), self.y.eval(ctx), self.z.eval(ctx)]
    }
}

/// The buffer count [`CegExpr::eval`] keeps for `y`/`x`. The shipped
/// data names only buffer 0 (`y0` / `x0`, `touhou.tdf:75`); the rest is
/// room for a def that wants to hold a second value.
const CEG_VALUE_BUFFERS: usize = 4;

impl CegExpr {
    /// Run the program. An empty one is `0.0`, upstream's "empty
    /// accumulator" start state — which is why a def with nothing to say
    /// is `Lit(0.0)` rather than an empty `Ops`.
    ///
    /// `Yank` / `MulBuf` follow upstream `ParseExplosionCode`: `y<n>`
    /// parks the accumulator in buffer *n* and starts over from zero,
    /// `x<n>` scales the accumulator by what is parked there. The parser
    /// this replaced treated both as no-ops, so `touhou.tdf:75`'s bubble
    /// trail is the one def whose shape changes (§3.2). An index past
    /// [`CEG_VALUE_BUFFERS`] is ignored rather than wrapped into a slot
    /// the def did not name.
    pub fn eval(&self, ctx: &EvalCtx) -> f32 {
        let Self::Ops(ops) = self else {
            return self.literal().unwrap_or(0.0);
        };
        let mut acc = 0.0;
        let mut buffers = [0.0; CEG_VALUE_BUFFERS];
        for op in ops.iter() {
            match *op {
                CegOp::Add(v) => acc += v,
                CegOp::Rand(v) => acc += ctx.rand01 * v,
                CegOp::Index(v) => acc += ctx.index as f32 * v,
                CegOp::Damage(v) => acc += ctx.damage * v,
                CegOp::Yank(n) => {
                    if let Some(slot) = buffers.get_mut(n as usize) {
                        *slot = acc;
                        acc = 0.0;
                    }
                }
                CegOp::MulBuf(n) => {
                    if let Some(slot) = buffers.get(n as usize) {
                        acc *= *slot;
                    }
                }
            }
        }
        acc
    }
}

// ─── Textures ───────────────────────────────────────────────────────

/// The texture aliases behind the names a TDF or the code writes —
/// upstream `RESOURCES.TDF`'s `kpsfx/` subset, the mapping the engine's
/// `GetTexture` did. 34 of upstream's 47: every one the shipped CEGs
/// (18), the weapon TDFs' `texture1` / `texture2` or a `meshes.rs` call
/// reaches, plus `squaretrans`, `arrownoends` and `hexastar`, which
/// upstream defines and the old table listed and nothing here names.
/// `gen_ceg_defs` panics rather than inventing a variant if a CEG names
/// an alias this enum does not have.
///
/// `none` is deliberately not a variant. Upstream resolves it to
/// `kpsfx/none.tga`, a blank bitmap; here it is `Option::None` on every
/// field that has one, which is the same picture (§3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, strum::IntoStaticStr, strum::VariantArray)]
pub enum CegTexture {
    #[strum(serialize = "circle")]
    Circle,
    #[strum(serialize = "hcircle")]
    Hcircle,
    #[strum(serialize = "square")]
    Square,
    #[strum(serialize = "squarehollow")]
    SquareHollow,
    #[strum(serialize = "squaretrans")]
    SquareTrans,
    #[strum(serialize = "hline")]
    HLine,
    #[strum(serialize = "vline")]
    VLine,
    #[strum(serialize = "dosray")]
    DosRay,
    #[strum(serialize = "arrow")]
    Arrow,
    #[strum(serialize = "arrownoends")]
    ArrowNoEnds,
    #[strum(serialize = "arrowflare")]
    ArrowFlare,
    #[strum(serialize = "bytelaser")]
    ByteLaser,
    #[strum(serialize = "bytelasermid")]
    ByteLaserMid,
    #[strum(serialize = "heart")]
    Heart,
    #[strum(serialize = "shockwave")]
    Shockwave,
    #[strum(serialize = "black")]
    Black,
    #[strum(serialize = "linkbeam")]
    LinkBeam,
    #[strum(serialize = "hexgrid")]
    HexGrid,
    #[strum(serialize = "hexgridhole")]
    HexGridHole,
    #[strum(serialize = "hexastar")]
    HexAStar,
    /// A second name for [`Self::HexAStar`]'s file, used by the
    /// MineLauncher's own CEG.
    #[strum(serialize = "minelauncher")]
    MineLauncher,
    #[strum(serialize = "pointertrail")]
    PointerTrail,
    #[strum(serialize = "flowtrail")]
    FlowTrail,
    #[strum(serialize = "firetrail")]
    FireTrail,
    #[strum(serialize = "sparkle")]
    Sparkle,
    #[strum(serialize = "bubbles")]
    Bubbles,
    #[strum(serialize = "lobedincantation")]
    LobedIncantation,
    /// Upstream `kpsfx/bullet.png`. The bake takes `.tga` only
    /// (`bundle.rs`), so this one still resolves to nothing at load and
    /// the layer that names it draws nothing; the variant is here
    /// because the data names the alias.
    #[strum(serialize = "bullet")]
    Bullet,
    /// The Rock-Paper-Scissors units' spawn drops. Bundled, and named
    /// only by `rock_fountain` / `paper_fountain` / `scissors_fountain`
    /// — which nothing in the shipped data references.
    #[strum(serialize = "minirock")]
    MiniRock,
    #[strum(serialize = "minipaper")]
    MiniPaper,
    #[strum(serialize = "miniscissors")]
    MiniScissors,
    // Engine default atlas (`ProjectileDrawer`): the beam body, its end
    // caps (also `explspike` streaks) and the BeamLaser emitter flare.
    #[strum(serialize = "laserfalloff")]
    LaserFalloff,
    #[strum(serialize = "laserend")]
    LaserEnd,
    #[strum(serialize = "flare")]
    Flare,
}

/// Every alias, in declaration order — the list [`resolve_texture`]
/// scans. `strum::VariantArray` gives it, so a new variant cannot be
/// left out of the lookup.
pub const ALL_CEG_TEXTURES: &[CegTexture] = CegTexture::VARIANTS;

impl CegTexture {
    /// The alias as the TDF authors it.
    pub fn name(self) -> &'static str {
        self.into()
    }

    /// The bitmap the alias stands for, spelled as the unit bundle keys
    /// it (`build.rs` bakes upstream's `bitmaps/kpsfx/*.tga` into
    /// `units.kpu`, and `bundle().texture` is where the name is looked
    /// up). Not one-to-one: [`Self::MineLauncher`] and [`Self::HexAStar`]
    /// are the same `hexastar.tga`.
    pub const fn file(self) -> &'static str {
        match self {
            Self::Circle => "whitecircle.tga",
            Self::Hcircle => "hollowcircle.tga",
            Self::Square => "solidwhite.tga",
            Self::SquareHollow => "hollowsquare.tga",
            Self::SquareTrans => "transparentwhite.tga",
            Self::HLine => "horizontalline.tga",
            Self::VLine => "verticalline.tga",
            Self::DosRay => "dosray.tga",
            Self::Arrow => "arrow.tga",
            Self::ArrowNoEnds => "arrownoends.tga",
            Self::ArrowFlare => "arrowflare.tga",
            Self::ByteLaser => "bytemegabeam.tga",
            Self::ByteLaserMid => "bytemegabeammid.tga",
            Self::Heart => "heart.tga",
            Self::Shockwave => "shockwave.tga",
            Self::Black => "black.tga",
            Self::LinkBeam => "linkbeam.tga",
            Self::HexGrid => "hexgrid.tga",
            Self::HexGridHole => "hexgridhole.tga",
            Self::HexAStar | Self::MineLauncher => "hexastar.tga",
            Self::PointerTrail => "pointershottrail.tga",
            Self::FlowTrail => "flowtrail1.tga",
            Self::FireTrail => "firetrail.tga",
            Self::Sparkle => "sparkle.tga",
            Self::Bubbles => "bubbles.tga",
            Self::LobedIncantation => "lobedincantation.tga",
            Self::Bullet => "bullet.png",
            Self::MiniRock => "minirock.tga",
            Self::MiniPaper => "minipaper.tga",
            Self::MiniScissors => "miniscissors.tga",
            Self::LaserFalloff => "laserfalloff.tga",
            Self::LaserEnd => "laserend.tga",
            Self::Flare => "flare.tga",
        }
    }
}

/// Resolve a texture name as a TDF writes it — trimmed, any case — to
/// the alias it means. `None` for `none`, for the empty string (the TDF
/// default) and for a name the engine's table never had.
///
/// Still a `&str` in, because the weapon TDFs name their `texture1` /
/// `texture2` in text and are the next step's business
/// (`docs/ceg-in-source-plan.md` §2.4); the CEG data itself no longer
/// comes through here.
pub fn resolve_texture(name: &str) -> Option<CegTexture> {
    let name = name.trim();
    ALL_CEG_TEXTURES
        .iter()
        .copied()
        .find(|tex| tex.name().eq_ignore_ascii_case(name))
}

/// The upstream explosion TDFs as `(file name, text)`, in name order.
///
/// Nothing at runtime reads these any more: every def the CEG runtime
/// needs is a `static` in [`super::ceg_defs`], and `build.rs` no longer
/// embeds the files, so they cost the binary nothing. They stay because
/// they are what those statics were generated *from* and what
/// [`cegs::tests::every_source_def_matches_the_parsed_tdf`](super::cegs::tests)
/// still checks them against, so the test build reads them off disk the
/// way `audio`'s tests read `assets/sounds`.
#[cfg(test)]
pub(super) fn explosion_tdfs() -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/defs/explosions");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
                .path()
        })
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tdf"))
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .unwrap_or_else(|| panic!("{}: no file name", path.display()))
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            (name, text)
        })
        .collect()
}

/// [`explosion_tdfs`] parsed and merged, which is what
/// `defs::parse_all` did for the registry this replaced.
#[cfg(test)]
pub(super) fn parsed_explosion_tdfs() -> spring_tdf::ExplosionDefs {
    let mut all = spring_tdf::ExplosionDefs::default();
    for (name, text) in explosion_tdfs() {
        let tdf = spring_tdf::Tdf::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        all.merge(spring_tdf::ExplosionDefs::from_tdf(&tdf));
    }
    all
}

#[cfg(test)]
mod tests {
    use super::super::cegs::ALL_CEG_KINDS;
    use super::*;
    use crate::units::content::defs;
    use spring_tdf as parsed;

    /// Every alias names a bitmap the unit bundle actually ships, so a
    /// typo in [`CegTexture::file`] fails here rather than turning a
    /// layer invisible at runtime — which is what an alias missing from
    /// the old table did to the four names added alongside this enum.
    ///
    /// `bullet` is the one exception, and it is honest about it:
    /// upstream's is `kpsfx/bullet.png` and the bake takes `.tga`
    /// (`bundle.rs`), so that layer draws nothing until the bake does.
    #[test]
    fn every_ceg_texture_names_a_bundled_bitmap() {
        for &texture in ALL_CEG_TEXTURES {
            if texture == CegTexture::Bullet {
                continue;
            }
            assert!(
                crate::units::content::bundle::bundle().has_texture(texture.file()),
                "{texture:?} names {}, which is not in the bundle",
                texture.file()
            );
        }
    }

    /// The runtime samples the static map itself — `&'static` stops, no
    /// allocation per particle — so [`ColorMap::sample`] is a copy of the
    /// parser's. Pin the two against each other: a drift between them is
    /// a CEG that colours differently than its `colorMap` says.
    #[test]
    fn color_map_sample_matches_the_parsed_one() {
        let cases: &[&[[f32; 4]]] = &[
            &[],
            &[[1.0, 0.5, 0.0, 1.0]],
            &[[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 0.25]],
            &[
                [1.0, 1.0, 1.0, 0.1],
                [0.2, 0.2, 1.0, 0.1],
                [0.0, 0.0, 0.0, 0.0],
            ],
            &[
                [0.1, 0.2, 0.3, 0.4],
                [0.5, 0.6, 0.7, 0.8],
                [0.9, 0.8, 0.7, 0.6],
                [0.5, 0.4, 0.3, 0.2],
            ],
        ];
        for &stops in cases {
            let parsed_map = parsed::ColorMap {
                stops: stops.to_vec(),
            };
            for t in [-1.0, 0.0, 0.2, 0.5, 0.75, 1.0, 2.0] {
                assert_eq!(
                    ColorMap(stops).sample(t),
                    parsed_map.sample(t),
                    "{stops:?} at {t}"
                );
            }
        }
    }

    /// The alias table as the names reach it from a file: `circle` is
    /// `whitecircle.tga`, and the file name is what `load_beam_texture`
    /// and the bundle's texture table are keyed by.
    #[test]
    fn resolve_texture_maps_aliases_to_files() {
        for (authored, want) in [
            ("circle", CegTexture::Circle),
            ("square", CegTexture::Square),
            ("hline", CegTexture::HLine),
            ("bytelaser", CegTexture::ByteLaser),
            ("pointertrail", CegTexture::PointerTrail),
            ("flowtrail", CegTexture::FlowTrail),
        ] {
            assert_eq!(resolve_texture(authored), Some(want));
            assert_eq!(
                resolve_texture(authored).map(CegTexture::file),
                Some(want.file())
            );
        }
    }

    #[test]
    fn resolve_texture_ignores_case_and_padding() {
        assert_eq!(resolve_texture("CIRCLE"), Some(CegTexture::Circle));
        assert_eq!(resolve_texture("  square  "), Some(CegTexture::Square));
    }

    /// `none` is `None`, not a bitmap (§3.3), and so are the empty
    /// string the TDFs default to and a name the table never had.
    #[test]
    fn resolve_texture_none_and_empty_skip() {
        assert_eq!(resolve_texture("none"), None);
        assert_eq!(resolve_texture(""), None);
        assert_eq!(resolve_texture("nonsense"), None);
    }

    /// Regenerate `src/units/content/ceg_defs/` from
    /// `assets/defs/explosions/*.tdf`, then `cargo fmt --all`.
    ///
    /// This is the writer and nothing more. The proof that what it wrote
    /// says what the TDF says is
    /// [`cegs::tests::every_source_def_matches_the_parsed_tdf`](super::super::cegs::tests),
    /// which reads the statics back into the parsed tree and compares —
    /// without going through a single line of the text emitted here, so a
    /// transcription slip fails the test instead of being reproduced by
    /// it.
    ///
    /// Layout is deliberately dumb (one field per line, trailing commas,
    /// no attempt at fitting) because `rustfmt` gets the last word.
    #[test]
    #[ignore = "rewrites committed source; run it deliberately"]
    fn gen_ceg_defs() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/units/content/ceg_defs");
        std::fs::create_dir_all(dir).expect(dir);
        let refs = references();
        // Which file and static each variant ended up in, to print the
        // `CegKind::def` match afterwards.
        let mut owner: Vec<Option<(String, String)>> = vec![None; ALL_CEG_KINDS.len()];
        let mut stems = Vec::new();
        let mut sections = 0;

        for (file, text) in explosion_tdfs() {
            let stem = file
                .strip_suffix(".tdf")
                .unwrap_or_else(|| panic!("{file}"))
                .to_owned();
            let tdf = parsed::Tdf::parse(&text).unwrap_or_else(|e| panic!("{file}: {e}"));
            let mut body = String::new();
            for (name, def) in &parsed::ExplosionDefs::from_tdf(&tdf).explosions {
                let kind = CegKind::find(name)
                    .unwrap_or_else(|| panic!("{file}: [{name}] is not a CegKind"));
                let static_name = name.to_ascii_uppercase();
                owner[kind.index()] = Some((stem.clone(), static_name.clone()));
                sections += 1;
                body.push_str(&section(name, def, kind, &refs));
            }
            assert!(!body.is_empty(), "{file} defines no CEG");
            std::fs::write(format!("{dir}/{stem}.rs"), module(&file, &body)).expect("write module");
            stems.push(stem);
        }

        std::fs::write(format!("{dir}/mod.rs"), mod_file(&stems)).expect("write mod.rs");

        let orphans: Vec<&str> = ALL_CEG_KINDS
            .iter()
            .filter(|&&kind| owner[kind.index()].is_none())
            .map(|&kind| kind.name())
            .collect();
        assert!(orphans.is_empty(), "no TDF file defines {orphans:?}");
        println!("// {} sections in {} files\n", sections, stems.len());
        for &kind in ALL_CEG_KINDS {
            let (stem, name) = owner[kind.index()].as_ref().expect("checked above");
            println!("            Self::{kind:?} => &super::ceg_defs::{stem}::{name},");
        }
    }

    /// One module's whole file: a header saying where it came from, the
    /// imports its body actually uses (an unused glob is a warning, and
    /// `[none]` uses almost nothing), and the statics.
    fn module(file: &str, body: &str) -> String {
        let mut src = format!(
            "//! `{file}` — the CEGs upstream ships in this file, as Rust.\n\
             //!\n\
             //! Generated from `assets/defs/explosions/{file}` by the ignored\n\
             //! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by\n\
             //! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;\n\
             //! a hand-edit that changes a value fails that test.\n\n\
             use crate::units::content::ceg_def::*;\n"
        );
        if body.contains("Lit(") || body.contains("Ops(") {
            src.push_str("use crate::units::content::ceg_def::CegExpr::*;\n");
        }
        if ["Particle(", "Flame(", "Spawner(", "Spike("]
            .iter()
            .any(|name| body.contains(name))
        {
            src.push_str("use crate::units::content::ceg_def::EffectProperties::*;\n");
        }
        if body.contains("Ops(") {
            src.push_str("use crate::units::content::ceg_def::CegOp::*;\n");
        }
        if body.contains("CegKind::") {
            src.push_str("use crate::units::content::cegs::CegKind;\n");
        }
        src.push('\n');
        src.push_str(body);
        src
    }

    /// The generated modules, in catalog order.
    fn mod_file(stems: &[String]) -> String {
        let mut src = String::from(
            "//! The CEG definitions, one module per\n\
             //! `assets/defs/explosions/*.tdf`, reached through\n\
             //! [`CegKind::def`](super::cegs::CegKind::def).\n\
             //!\n\
             //! Generated as a whole by the ignored `gen_ceg_defs` test in\n\
             //! `super::ceg_def`; see one module's header for what that means.\n",
        );
        for stem in stems {
            src.push_str(&format!("pub mod {stem};\n"));
        }
        src
    }

    /// One `[name]` section: a doc line saying what it is and who calls
    /// it, then the def.
    fn section(name: &str, def: &parsed::ExplosionDef, kind: CegKind, refs: &References) -> String {
        let mut out = format!("/// `[{name}]` — {}.\n", summary(def));
        out.push_str(&format!("/// {}\n", refs.line(kind)));
        out.push_str(&format!(
            "pub static {}: ExplosionDef = ExplosionDef {{\n",
            name.to_ascii_uppercase()
        ));
        if def.effects.is_empty() {
            out.push_str("    effects: &[],\n");
        } else {
            out.push_str("    effects: &[\n");
            for layer in &def.effects {
                out.push_str(&indent(&layer_src(layer, name), 4));
                out.push_str(",\n");
            }
            out.push_str("    ],\n");
        }
        match &def.ground_flash {
            None => out.push_str("    ground_flash: None,\n"),
            Some(gf) if gf.ttl <= 0.0 => {
                out.push_str(&format!(
                    "    // The `[groundflash]` block is `ttl = {}`: the engine ages it\n\
                     \x20   // by nothing per frame, so it draws nothing (§3.4).\n",
                    lit(gf.ttl)
                ));
                out.push_str("    ground_flash: None,\n");
            }
            Some(gf) => {
                out.push_str("    ground_flash: Some(GroundFlash {\n");
                field(&mut out, "flash_size", &lit(gf.flash_size));
                field(&mut out, "flash_alpha", &lit(gf.flash_alpha));
                field(&mut out, "circle_growth", &lit(gf.circle_growth));
                field(&mut out, "circle_alpha", &lit(gf.circle_alpha));
                field(&mut out, "ttl", &lit(gf.ttl));
                field(
                    &mut out,
                    "color",
                    &format!(
                        "[{}, {}, {}]",
                        lit(gf.color[0]),
                        lit(gf.color[1]),
                        lit(gf.color[2])
                    ),
                );
                out.push_str("    }),\n");
            }
        }
        out.push_str("};\n\n");
        out
    }

    /// The doc line's first half: how big the def is, and whether its
    /// ground flash is one the engine draws.
    fn summary(def: &parsed::ExplosionDef) -> String {
        let layers = def.effects.len();
        let mut out = format!(
            "{layers} effect {}",
            if layers == 1 { "layer" } else { "layers" }
        );
        match &def.ground_flash {
            None => out.push_str(", no ground flash"),
            Some(gf) if gf.ttl > 0.0 => out.push_str(", ground flash live"),
            Some(_) => out.push_str(", ground flash `ttl = 0`"),
        }
        out
    }

    /// One `[properties]` block, in the order the TDF's typed reader
    /// fills it, so a reviewer can walk the two side by side.
    fn layer_src(layer: &parsed::EffectLayer, section: &str) -> String {
        let mut out = String::from("EffectLayer {\n");
        field(&mut out, "name", &format!("{:?}", layer.name));
        field(&mut out, "air", &layer.air.to_string());
        field(&mut out, "ground", &layer.ground.to_string());
        field(&mut out, "water", &layer.water.to_string());
        field(&mut out, "count", &layer.count.to_string());
        out.push_str("    properties: ");
        out.push_str(&properties(&layer.properties, section));
        out.push_str("),\n}\n");
        out
    }

    fn properties(props: &parsed::EffectProperties, section: &str) -> String {
        let mut out = String::new();
        match props {
            parsed::EffectProperties::Particle(p) => {
                out.push_str("Particle(ParticleProperties {\n");
                field(&mut out, "texture", &texture(&p.texture, section));
                field(&mut out, "color_map", &color_map(&p.color_map));
                field(&mut out, "num_particles", &value(&p.num_particles));
                field(&mut out, "particle_life", &value(&p.particle_life));
                field(
                    &mut out,
                    "particle_life_spread",
                    &value(&p.particle_life_spread),
                );
                field(&mut out, "particle_speed", &value(&p.particle_speed));
                field(
                    &mut out,
                    "particle_speed_spread",
                    &value(&p.particle_speed_spread),
                );
                field(&mut out, "particle_size", &value(&p.particle_size));
                field(
                    &mut out,
                    "particle_size_spread",
                    &value(&p.particle_size_spread),
                );
                field(&mut out, "size_growth", &value(&p.size_growth));
                field(&mut out, "size_mod", &value(&p.size_mod));
                field(&mut out, "emit_rot", &value(&p.emit_rot));
                field(&mut out, "emit_rot_spread", &value(&p.emit_rot_spread));
                field(&mut out, "airdrag", &value(&p.airdrag));
                field(&mut out, "gravity", &vector(&p.gravity));
                field(&mut out, "pos", &vector(&p.pos));
                field(&mut out, "emit_vector", &emit_vector(&p.emit_vector));
                field(&mut out, "directional", &p.directional.to_string());
                field(&mut out, "always_visible", &p.always_visible.to_string());
                out.push('}');
            }
            parsed::EffectProperties::Flame(p) => {
                out.push_str("Flame(FlameProperties {\n");
                field(&mut out, "side_texture", &texture(&p.side_texture, section));
                field(
                    &mut out,
                    "front_texture",
                    &texture(&p.front_texture, section),
                );
                field(&mut out, "color_map", &color_map(&p.color_map));
                field(&mut out, "size", &value(&p.size));
                field(&mut out, "length", &value(&p.length));
                field(&mut out, "size_growth", &value(&p.size_growth));
                field(&mut out, "ttl", &value(&p.ttl));
                field(&mut out, "front_offset", &value(&p.front_offset));
                field(&mut out, "speed", &value(&p.speed));
                field(&mut out, "dir", &vector(&p.dir));
                field(&mut out, "pos", &vector(&p.pos));
                field(&mut out, "always_visible", &p.always_visible.to_string());
                out.push('}');
            }
            parsed::EffectProperties::Spawner(p) => {
                out.push_str("Spawner(SpawnerProperties {\n");
                let target = CegKind::find(&p.explosion_generator).unwrap_or_else(|| {
                    panic!(
                        "[{section}]: spawner target {:?} is not a CegKind",
                        p.explosion_generator
                    )
                });
                field(&mut out, "target", &format!("CegKind::{target:?}"));
                field(&mut out, "delay", &value(&p.delay));
                field(&mut out, "damage", &value(&p.damage));
                field(&mut out, "dir", &vector(&p.dir));
                field(&mut out, "pos", &vector(&p.pos));
                field(&mut out, "speed", &vector(&p.speed));
                field(&mut out, "always_visible", &p.always_visible.to_string());
                out.push('}');
            }
            parsed::EffectProperties::Spike(p) => {
                out.push_str("Spike(SpikeProperties {\n");
                field(&mut out, "length", &value(&p.length));
                field(&mut out, "width", &value(&p.width));
                field(&mut out, "alpha", &value(&p.alpha));
                field(&mut out, "alpha_decay", &value(&p.alpha_decay));
                field(&mut out, "dir", &vector(&p.dir));
                field(&mut out, "pos", &vector(&p.pos));
                field(
                    &mut out,
                    "color",
                    &match &p.color {
                        None => "None".to_string(),
                        Some(c) => format!("Some({})", vector(c)),
                    },
                );
                out.push('}');
            }
            parsed::EffectProperties::Raw(entries) => panic!(
                "[{section}] is a class the engine does not draw ({} keys of raw \
                 entries): the source form has no place for it, so it has to be \
                 modelled or the layer dropped from the data first",
                entries.len()
            ),
        }
        out
    }

    fn emit_vector(e: &parsed::EmitVector) -> String {
        match e {
            parsed::EmitVector::Direction => "EmitVector::Direction".to_string(),
            parsed::EmitVector::Literal(v) => {
                format!("EmitVector::Literal({})", vector(v))
            }
        }
    }

    fn vector(v: &parsed::CegVec3) -> String {
        if let (Some(x), Some(y), Some(z)) = (constant(&v.x), constant(&v.y), constant(&v.z)) {
            if x == 0.0 && y == 0.0 && z == 0.0 {
                return "ZERO".to_string();
            }
            return format!("vec3({}, {}, {})", lit(x), lit(y), lit(z));
        }
        format!(
            "CegVec3 {{ x: {}, y: {}, z: {} }}",
            value(&v.x),
            value(&v.y),
            value(&v.z)
        )
    }

    fn color_map(map: &parsed::ColorMap) -> String {
        let rows: Vec<String> = map
            .stops
            .iter()
            .map(|stop| {
                format!(
                    "[{}, {}, {}, {}]",
                    lit(stop[0]),
                    lit(stop[1]),
                    lit(stop[2]),
                    lit(stop[3])
                )
            })
            .collect();
        format!("ColorMap(&[{}])", rows.join(", "))
    }

    fn texture(name: &str, section: &str) -> String {
        let name = name.trim();
        if name.is_empty() || name.eq_ignore_ascii_case("none") {
            return "None".to_string();
        }
        resolve_texture(name)
            .map(|tex| format!("Some(CegTexture::{tex:?})"))
            .unwrap_or_else(|| panic!("[{section}]: texture {name:?} has no CegTexture variant"))
    }

    /// `Lit` when the whole program is plain adds — the sum is then all
    /// it computes, in the same order `eval` would — and the program
    /// itself otherwise.
    ///
    /// `parsed::CegExpr::literal()` is not usable here: it counts the
    /// opcodes the parser does not model as literal as well, so folding
    /// with it would drop the `y0` / `x0` of `touhou.tdf:75` and leave
    /// the first add standing alone.
    fn value(e: &parsed::CegExpr) -> String {
        match constant(e) {
            Some(v) => format!("Lit({})", lit(v)),
            None => format!(
                "Ops(&[{}])",
                e.0.iter().map(op).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    fn constant(e: &parsed::CegExpr) -> Option<f32> {
        let mut sum = 0.0;
        for op in &e.0 {
            match op {
                parsed::CegOp::Add(v) => sum += v,
                _ => return None,
            }
        }
        Some(sum)
    }

    fn op(o: &parsed::CegOp) -> String {
        match *o {
            parsed::CegOp::Add(v) => format!("Add({})", lit(v)),
            parsed::CegOp::Rand(v) => format!("Rand({})", lit(v)),
            parsed::CegOp::Index(v) => format!("Index({})", lit(v)),
            parsed::CegOp::Damage(v) => format!("Damage({})", lit(v)),
            parsed::CegOp::Other('y', n) => format!("Yank({})", buffer(n, 'y')),
            parsed::CegOp::Other('x', n) => format!("MulBuf({})", buffer(n, 'x')),
            parsed::CegOp::Other(c, _) => panic!(
                "CEG opcode '{c}' is not in the source op set: add the variant \
                 (docs/ceg-in-source-plan.md §3.2) or take the op out of the data"
            ),
        }
    }

    /// An opcode's buffer number, which the parser hands over as an
    /// `f32` because it never looked at it either.
    fn buffer(n: f32, opcode: char) -> u8 {
        let index = n as i32;
        assert!(
            n == index as f32 && (0..CEG_VALUE_BUFFERS as i32).contains(&index),
            "'{opcode}{n}' is not a buffer this crate has room for"
        );
        index as u8
    }

    fn field(out: &mut String, name: &str, value: &str) {
        out.push_str(&format!("    {name}: {value},\n"));
    }

    fn indent(block: &str, spaces: usize) -> String {
        block
            .lines()
            .map(|line| format!("{}{line}\n", " ".repeat(spaces)))
            .collect()
    }

    /// A Rust literal: `{:?}` is the one float formatter that keeps the
    /// `.0` an integer value needs to stay an `f32`.
    fn lit(v: f32) -> String {
        format!("{v:?}")
    }

    /// Who references each CEG, for the doc line on each static: the
    /// census is what tells a reader whether a def is load-bearing.
    struct References {
        weapons: Vec<usize>,
        sfx: Vec<usize>,
        spawners: Vec<usize>,
    }

    impl References {
        fn line(&self, kind: CegKind) -> String {
            let i = kind.index();
            let mut parts = Vec::new();
            for (count, one, many) in [
                (self.weapons[i], "weapon", "weapons"),
                (self.sfx[i], "`[SFXTypes]` entry", "`[SFXTypes]` entries"),
                (self.spawners[i], "spawner", "spawners"),
            ] {
                if count > 0 {
                    parts.push(format!("{count} {}", if count == 1 { one } else { many }));
                }
            }
            if parts.is_empty() {
                "Referenced by nothing in the shipped data.".to_string()
            } else {
                format!("Referenced by {}.", parts.join(", "))
            }
        }
    }

    fn references() -> References {
        let defs = defs::defs();
        let zero = || vec![0; ALL_CEG_KINDS.len()];
        let mut refs = References {
            weapons: zero(),
            sfx: zero(),
            spawners: zero(),
        };
        for weapon in defs.weapons.weapons.values() {
            let generator = CegKind::find(&weapon.explosion_generator);
            let tag = CegKind::find(&weapon.ceg_tag).filter(|kind| Some(*kind) != generator);
            for kind in generator.into_iter().chain(tag) {
                refs.weapons[kind.index()] += 1;
            }
        }
        for unit in defs.units.units.values() {
            for reference in &unit.sfx_types {
                if let Some(kind) = CegKind::find(reference) {
                    refs.sfx[kind.index()] += 1;
                }
            }
        }
        for def in parsed_explosion_tdfs().explosions.values() {
            for layer in &def.effects {
                if let parsed::EffectProperties::Spawner(props) = &layer.properties
                    && let Some(kind) = CegKind::find(&props.explosion_generator)
                {
                    refs.spawners[kind.index()] += 1;
                }
            }
        }
        refs
    }
}
