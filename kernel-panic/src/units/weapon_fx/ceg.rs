//! Custom Explosion Generator (CEG) runtime.
//!
//! Parsing now lives in [`spring_tdf::ExplosionDefs`] (typed `CegExpr`,
//! `ColorMap`, `EmitVector`, …). This module keeps only:
//!
//! - [`CegRegistry`]: loads every `gamedata/explosions/*.tdf` into a
//!   merged [`spring_tdf::ExplosionDefs`] and exposes atlas-alias
//!   resolution (`circle` → `whitecircle.tga`).
//! - [`spawn_ceg`]: walks an [`ExplosionDef`]'s effect layers and
//!   instantiates the right runtime object for each class —
//!   `CSimpleParticleSystem` becomes a batch of [`CegParticle`]s,
//!   `CBitmapMuzzleFlame` becomes a [`CegFlame`] billboard,
//!   `CExpGenSpawner` becomes a [`CegDelayedSpawn`] timer that replays
//!   its target CEG on fire.
//! - [`tick_ceg_particles`] + [`tick_ceg_flames`] + [`tick_ceg_delayed_spawns`]:
//!   per-frame integration + despawn.
//!
//! All ambient "spread" math (`X rN`, `X iN`, `X dN`) is handled by
//! [`CegExpr::eval`] with an [`EvalCtx`] populated per particle — the
//! runtime never touches the raw TDF strings.

use bevy::prelude::*;
use spring_tdf::{
    CegExpr, EffectProperties, EmitVector, EvalCtx, ExplosionDef, ExplosionDefs, FlameProperties,
    ParticleProperties, SpawnerProperties,
};

use crate::units::assets::meshes::{S3OModelCache, load_beam_texture};
use crate::units::content::tdf_loader;

/// Registry of every parsed CEG. Internally wraps
/// [`spring_tdf::ExplosionDefs`] so parsing stays in one place.
#[derive(Resource, Default)]
pub struct CegRegistry {
    defs: ExplosionDefs,
}

/// Shared particle quad mesh: corners at `±1` on X and Y so the
/// spawner can set `Transform::scale = Vec3::splat(size)` and get
/// upstream's `±xdir*size ± ydir*size` total extent (full width =
/// `2 * size`). Using `Rectangle::new(1.0, 1.0)` instead would halve
/// every particle relative to its CEG-authored size — that's the bug
/// that made every impact look puny vs. upstream footage.
#[derive(Resource, Default)]
pub(super) struct CegParticleMesh {
    pub handle: Option<Handle<Mesh>>,
}

impl CegParticleMesh {
    fn get(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.handle
            .get_or_insert_with(|| meshes.add(Rectangle::new(2.0, 2.0)))
            .clone()
    }
}

impl CegRegistry {
    /// Load every `explosions/*.tdf` under `upstream/Kernel-Panic/`.
    pub fn load() -> Self {
        let Some(dir) = tdf_loader::find_upstream_dir("gamedata/explosions") else {
            warn!("Upstream CEG directory not found — using empty registry");
            return Self::default();
        };

        let mut defs = ExplosionDefs::default();
        for (_filename, tdf) in tdf_loader::load_all_tdf_files(&dir, "tdf") {
            defs.merge(ExplosionDefs::from_tdf(&tdf));
        }

        info!(
            "CEG registry: {} explosion generators loaded",
            defs.explosions.len()
        );
        Self { defs }
    }

    /// Look up a CEG by section name. Accepts both the raw name and the
    /// `custom:NAME` prefix the weapon TDFs use.
    pub fn get(&self, name: &str) -> Option<&ExplosionDef> {
        self.defs.get(name)
    }

    /// Re-shape the texture name the CEG authored (e.g. `circle`) into
    /// the on-disk filename via the same `RESOURCES.TDF` mapping the
    /// weapon atlases use. The subset reproduced here covers every
    /// texture referenced by CEGs KP actually ships; anything else
    /// returns `None`.
    pub fn resolve_texture(tex_name: &str) -> Option<&'static str> {
        match tex_name.trim().to_ascii_lowercase().as_str() {
            "circle" => Some("whitecircle.tga"),
            "hcircle" => Some("hollowcircle.tga"),
            "square" => Some("solidwhite.tga"),
            "squarehollow" => Some("hollowsquare.tga"),
            "squaretrans" => Some("transparentwhite.tga"),
            "hline" => Some("horizontalline.tga"),
            "vline" => Some("verticalline.tga"),
            "dosray" => Some("dosray.tga"),
            "arrow" => Some("arrow.tga"),
            "arrownoends" => Some("arrownoends.tga"),
            "arrowflare" => Some("arrowflare.tga"),
            "bytelaser" => Some("bytemegabeam.tga"),
            "bytelasermid" => Some("bytemegabeammid.tga"),
            "heart" => Some("heart.tga"),
            "shockwave" => Some("shockwave.tga"),
            "black" => Some("black.tga"),
            "linkbeam" => Some("linkbeam.tga"),
            "hexgrid" => Some("hexgrid.tga"),
            "hexgridhole" => Some("hexgridhole.tga"),
            "hexastar" => Some("hexastar.tga"),
            "pointertrail" => Some("pointershottrail.tga"),
            "firetrail" => Some("firetrail.tga"),
            "sparkle" => Some("sparkle.tga"),
            "bubbles" => Some("bubbles.tga"),
            "lobedincantation" => Some("lobedincantation.tga"),
            "none" | "" => None,
            _ => None,
        }
    }
}

// ─── Runtime components ─────────────────────────────────────────────

/// Upstream sim runs at 30 fps; frame-denominated CEG fields
/// (life/speed/gravity/sizegrowth) get multiplied by this to match
/// Bevy's per-second `Time::delta_secs` convention.
const CEG_FRAME_RATE: f32 = 30.0;

/// One live particle spawned from a `CSimpleParticleSystem`.
#[derive(Component)]
pub(super) struct CegParticle {
    pub velocity: Vec3,
    pub gravity: Vec3,
    pub airdrag_per_sec: f32,
    pub size: f32,
    pub size_growth_per_sec: f32,
    pub size_mod_per_sec: f32,
    pub color_map_stops: Vec<[f32; 4]>,
    pub life: f32,
    pub max_life: f32,
    pub material: Handle<StandardMaterial>,
    pub directional: bool,
}

/// One live `CBitmapMuzzleFlame` billboard — used for shockwaves
/// (expanding circle ring) and muzzle-exhaust cones.
#[derive(Component)]
pub(super) struct CegFlame {
    pub life_frames: f32,
    pub max_life_frames: f32,
    pub base_size: f32,
    pub size_growth_per_frame: f32,
    pub color_map_stops: Vec<[f32; 4]>,
    pub material: Handle<StandardMaterial>,
}

/// A scheduled recursive CEG spawn (CExpGenSpawner).
///
/// Each `count` iteration of the spawner becomes one `CegDelayedSpawn`
/// entity with its own `delay_frames` countdown. When the timer hits
/// zero the runtime calls `spawn_ceg` again with `target_ceg`; the
/// entity then despawns.
#[derive(Component)]
pub(super) struct CegDelayedSpawn {
    pub delay_secs: f32,
    pub pos: Vec3,
    pub dir: Vec3,
    pub target_ceg: String,
}

// ─── Spawning ───────────────────────────────────────────────────────

/// Spawn a CEG at `pos` with firing direction `dir`. Returns true iff
/// the CEG name was found; a missing entry lets callers fall back to a
/// synthesised burst.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_ceg(
    ceg_name: &str,
    pos: Vec3,
    dir: Vec3,
    registry: &CegRegistry,
    rng: &mut u32,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    particle_mesh: &mut CegParticleMesh,
) -> bool {
    let Some(def) = registry.get(ceg_name) else {
        return false;
    };

    let dir = dir.normalize_or(Vec3::Y);
    let mesh = particle_mesh.get(meshes);

    for effect in &def.effects {
        match &effect.properties {
            EffectProperties::Particle(p) => spawn_particle_system(
                effect.count,
                p,
                pos,
                dir,
                rng,
                commands,
                materials,
                images,
                model_cache,
                &mesh,
            ),
            EffectProperties::Flame(f) => spawn_flame(
                effect.count,
                f,
                pos,
                dir,
                commands,
                materials,
                images,
                model_cache,
                &mesh,
            ),
            EffectProperties::Spawner(s) => {
                spawn_delayed(effect.count, s, pos, dir, commands);
            }
            EffectProperties::Raw(_) => {
                // Unsupported class (CStars, etc.) — silently skipped.
            }
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn spawn_particle_system(
    count: u32,
    props: &ParticleProperties,
    origin: Vec3,
    dir: Vec3,
    rng: &mut u32,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    mesh: &Handle<Mesh>,
) {
    let Some(filename) = CegRegistry::resolve_texture(&props.texture) else {
        return;
    };
    let Some((tex, _w, _h)) = load_beam_texture(filename, model_cache, images) else {
        return;
    };

    let base_dir = match &props.emit_vector {
        EmitVector::Direction => dir,
        EmitVector::Literal(v) => {
            Vec3::from_array(v.eval(&EvalCtx::default())).normalize_or(Vec3::Y)
        }
    };

    for fire_index in 0..count {
        let n_particles = props
            .num_particles
            .eval(&EvalCtx {
                index: fire_index,
                ..Default::default()
            })
            .round()
            .max(1.0) as u32;

        for particle_index in 0..n_particles {
            let ctx_base = EvalCtx {
                index: fire_index * n_particles + particle_index,
                damage: 0.0,
                rand01: next_unit(rng),
            };

            let life_frames = eval_with_spread(
                &props.particle_life,
                &props.particle_life_spread,
                rng,
                ctx_base,
            )
            .max(1.0);
            let life_secs = life_frames / CEG_FRAME_RATE;

            let size = eval_with_spread(
                &props.particle_size,
                &props.particle_size_spread,
                rng,
                ctx_base,
            )
            .max(0.1);

            let speed_per_frame = eval_with_spread(
                &props.particle_speed,
                &props.particle_speed_spread,
                rng,
                ctx_base,
            );
            let speed_per_sec = speed_per_frame * CEG_FRAME_RATE;

            let rot_deg = eval_with_spread(&props.emit_rot, &props.emit_rot_spread, rng, ctx_base);
            let rot_rad = rot_deg.to_radians();
            let perp = perpendicular_to(base_dir, next_signed(rng));
            let rotated_dir = Quat::from_axis_angle(perp, rot_rad) * base_dir;
            let velocity = rotated_dir.normalize_or(Vec3::Y) * speed_per_sec;

            // Evaluate `pos` with a fresh rand per axis so `pos=-30 r60,
            // 1.0, -30 r60` samples the square uniformly instead of
            // landing on the same corner every particle.
            let pos_x = props.pos.x.eval(&EvalCtx {
                rand01: next_unit(rng),
                ..ctx_base
            });
            let pos_y = props.pos.y.eval(&EvalCtx {
                rand01: next_unit(rng),
                ..ctx_base
            });
            let pos_z = props.pos.z.eval(&EvalCtx {
                rand01: next_unit(rng),
                ..ctx_base
            });
            let particle_pos = origin + Vec3::new(pos_x, pos_y, pos_z);

            let gravity_per_frame = Vec3::from_array(props.gravity.eval(&ctx_base));
            let gravity_per_sec = gravity_per_frame * CEG_FRAME_RATE;

            let airdrag = props.airdrag.eval(&ctx_base);
            let airdrag_per_sec = if airdrag <= 0.0 || airdrag >= 1.0 {
                1.0
            } else {
                // v(t) = v0 * drag^(t*fps) → per-sec scale = drag^fps
                airdrag.powf(CEG_FRAME_RATE)
            };

            let size_mod = props.size_mod.eval(&ctx_base);
            let size_mod_per_sec = if size_mod > 0.0 && size_mod != 1.0 {
                size_mod.powf(CEG_FRAME_RATE)
            } else {
                1.0
            };
            let size_growth_per_sec = props.size_growth.eval(&ctx_base) * CEG_FRAME_RATE;

            // Material: `unlit=true` means StandardMaterial ignores
            // lighting, so we can drive colour entirely via `base_color`.
            // Upstream multiplies the texture by `colorMap[life]` with
            // no emissive boost — the colour IS the brightness under
            // additive blending. An emissive multiplier would over-
            // saturate every particle after tonemapping and turn the
            // faint blue shot1 puff into a flashbang.
            let initial = props.color_map.sample(0.0);
            let material = materials.add(StandardMaterial {
                base_color: Color::linear_rgba(initial[0], initial[1], initial[2], initial[3]),
                base_color_texture: Some(tex.clone()),
                unlit: true,
                alpha_mode: AlphaMode::Add,
                cull_mode: None,
                ..default()
            });

            commands.spawn((
                CegParticle {
                    velocity,
                    gravity: gravity_per_sec,
                    airdrag_per_sec,
                    size,
                    size_growth_per_sec,
                    size_mod_per_sec,
                    color_map_stops: props.color_map.stops.clone(),
                    life: life_secs,
                    max_life: life_secs,
                    material: material.clone(),
                    directional: props.directional,
                },
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material),
                Transform::from_translation(particle_pos).with_scale(Vec3::splat(size)),
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_flame(
    count: u32,
    props: &FlameProperties,
    origin: Vec3,
    _dir: Vec3,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    mesh: &Handle<Mesh>,
) {
    // We render only the `frontTexture` plane — the camera-facing
    // billboard — which is what every upstream shockwave
    // (`frontTexture=shockwave`, sideTexture=none) actually uses.
    let front = props.front_texture.trim();
    let Some(filename) = CegRegistry::resolve_texture(front) else {
        return;
    };
    let Some((tex, _w, _h)) = load_beam_texture(filename, model_cache, images) else {
        return;
    };

    let ctx = EvalCtx::default();
    let base_size = props.size.eval(&ctx).max(0.1);
    let size_growth_per_frame = props.size_growth.eval(&ctx);
    let ttl = props.ttl.eval(&ctx).max(1.0);
    let pos_offset = Vec3::from_array(props.pos.eval(&ctx));

    let initial = props.color_map.sample(0.0);
    let material = materials.add(StandardMaterial {
        base_color: Color::linear_rgba(initial[0], initial[1], initial[2], initial[3]),
        base_color_texture: Some(tex),
        unlit: true,
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        ..default()
    });

    for _ in 0..count {
        commands.spawn((
            CegFlame {
                life_frames: ttl,
                max_life_frames: ttl,
                base_size,
                size_growth_per_frame,
                color_map_stops: props.color_map.stops.clone(),
                material: material.clone(),
            },
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(origin + pos_offset).with_scale(Vec3::splat(base_size)),
        ));
    }
}

/// Queue `count` recursive spawns of `props.explosion_generator`, each
/// with its own `delay`-derived countdown. Evaluated via the `iN`
/// (per-spawn index) expression tokens so `delay=8 i8 count=240` fires
/// at 8, 16, 24, … frames as upstream intends.
fn spawn_delayed(
    count: u32,
    props: &SpawnerProperties,
    origin: Vec3,
    dir: Vec3,
    commands: &mut Commands,
) {
    if props.explosion_generator.is_empty() {
        return;
    }
    for i in 0..count {
        let ctx = EvalCtx {
            index: i,
            ..Default::default()
        };
        let delay_frames = props.delay.eval(&ctx).max(0.0);
        let delay_secs = delay_frames / CEG_FRAME_RATE;
        let pos_offset = Vec3::from_array(props.pos.eval(&ctx));
        commands.spawn(CegDelayedSpawn {
            delay_secs,
            pos: origin + pos_offset,
            dir,
            target_ceg: props.explosion_generator.clone(),
        });
    }
}

// ─── Tick ───────────────────────────────────────────────────────────

/// Physics + colour update for live CEG particles.
pub(super) fn tick_ceg_particles(
    time: Res<Time>,
    mut particles: Query<(Entity, &mut CegParticle, &mut Transform)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    camera_q: Query<&GlobalTransform, With<crate::rendering::camera::RtsCamera>>,
    mut commands: Commands,
) {
    if particles.is_empty() {
        return;
    }
    let dt = time.delta_secs();
    // Upstream `CSimpleParticleSystem::Draw` billboards using
    // `camera->GetRight()` and `camera->GetUp()` (or the forward-
    // crossed variant for directional). We take those axes from the
    // camera's GlobalTransform rather than deriving them from world-
    // Y crossed with the camera-to-particle vector — the world-Y
    // trick falls apart when the RTS camera pitches steeply.
    let (cam_pos, cam_right, cam_up, cam_fwd) = camera_q
        .single()
        .map(|gt| {
            let t = gt.compute_transform();
            (
                t.translation,
                t.right().as_vec3(),
                t.up().as_vec3(),
                -t.forward().as_vec3(),
            )
        })
        .unwrap_or_else(|_| (Vec3::Y * 1000.0, Vec3::X, Vec3::Y, Vec3::Z));

    for (entity, mut p, mut transform) in &mut particles {
        p.life -= dt;
        if p.life <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }

        if p.airdrag_per_sec > 0.0 && p.airdrag_per_sec != 1.0 {
            let drag_step = p.airdrag_per_sec.powf(dt);
            p.velocity *= drag_step;
        }
        let gravity = p.gravity;
        p.velocity += gravity * dt;
        let velocity = p.velocity;
        transform.translation += velocity * dt;

        p.size += p.size_growth_per_sec * dt;
        if p.size_mod_per_sec > 0.0 && p.size_mod_per_sec != 1.0 {
            p.size *= p.size_mod_per_sec.powf(dt);
        }
        transform.scale = Vec3::splat(p.size.max(0.01));

        // Build a rotation whose local axes map to the camera-facing
        // (xdir, ydir) pair upstream uses. The shared particle mesh
        // is a 2×2 rectangle in the XY plane; with cam_right → local X
        // and cam_up → local Y, the post-scale extent at each corner
        // matches `±xdir*size ± ydir*size` verbatim.
        let (right, up, normal) = if p.directional && p.velocity.length_squared() > 1e-3 {
            // `directional=1` (hline tracers): align Y with velocity,
            // X with the camera-facing perpendicular.
            let zdir = (transform.translation - cam_pos).normalize_or(-cam_fwd);
            let fwd = p.velocity.normalize();
            let xdir = zdir.cross(fwd).normalize_or(cam_right);
            let ydir = xdir.cross(zdir).normalize_or(fwd);
            (xdir, ydir, zdir)
        } else {
            (cam_right, cam_up, cam_fwd)
        };
        transform.rotation = Quat::from_mat3(&Mat3::from_cols(right, up, normal));

        let frac = 1.0 - (p.life / p.max_life).clamp(0.0, 1.0);
        let c = sample_stops(&p.color_map_stops, frac);
        if let Some(mat) = materials.get_mut(&p.material) {
            // Additive blend: final_color = texture × base_color, no
            // emissive multiplier. Matches upstream's GL_ONE/GL_ONE
            // pass where brightness comes from stacking translucent
            // particles rather than from per-particle amplification.
            mat.base_color = Color::linear_rgba(c[0], c[1], c[2], c[3]);
        }
    }
}

/// Drop the emissive-over-boost on the per-frame colour rewrite so
/// the shockwave's faint peach→transparent gradient looks like a
/// wispy ring rather than a pulsing ember.
///
/// Grow + fade `CBitmapMuzzleFlame` billboards. Ticked in sim-frame
/// terms: `size_growth` is authored per frame, `ttl` is authored in
/// frames, so we advance `life_frames` by `dt * CEG_FRAME_RATE`.
pub(super) fn tick_ceg_flames(
    time: Res<Time>,
    mut flames: Query<(Entity, &mut CegFlame, &mut Transform)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    camera_q: Query<&GlobalTransform, With<crate::rendering::camera::RtsCamera>>,
    mut commands: Commands,
) {
    if flames.is_empty() {
        return;
    }
    let dt_frames = time.delta_secs() * CEG_FRAME_RATE;
    let cam_pos = camera_q
        .single()
        .map(|gt| gt.translation())
        .unwrap_or(Vec3::Y * 1000.0);

    for (entity, mut f, mut transform) in &mut flames {
        f.life_frames -= dt_frames;
        if f.life_frames <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }

        // Grow: size = base + growth * frames_elapsed.
        let frames_elapsed = f.max_life_frames - f.life_frames;
        let size = f.base_size + f.size_growth_per_frame * frames_elapsed;
        transform.scale = Vec3::splat(size.max(0.01));

        // Face the camera.
        let to_cam = (cam_pos - transform.translation).normalize_or(Vec3::Z);
        let right = Vec3::Y.cross(to_cam).normalize_or(Vec3::X);
        let up = to_cam.cross(right).normalize_or(Vec3::Y);
        transform.rotation = Quat::from_mat3(&Mat3::from_cols(right, up, to_cam));

        let frac = 1.0 - (f.life_frames / f.max_life_frames).clamp(0.0, 1.0);
        let c = sample_stops(&f.color_map_stops, frac);
        if let Some(mat) = materials.get_mut(&f.material) {
            mat.base_color = Color::linear_rgba(c[0], c[1], c[2], c[3]);
        }
    }
}

/// Count down each pending recursive spawn and fire its target CEG when
/// the timer hits zero. Despawns the timer entity after firing.
#[allow(clippy::too_many_arguments)]
pub(super) fn tick_ceg_delayed_spawns(
    time: Res<Time>,
    mut timers: Query<(Entity, &mut CegDelayedSpawn)>,
    registry: Res<CegRegistry>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut particle_mesh: ResMut<CegParticleMesh>,
    mut rng: Local<u32>,
) {
    if timers.is_empty() {
        return;
    }
    let dt = time.delta_secs();

    // Clone out the ready targets so we can mutate commands freely.
    let mut ready: Vec<(Entity, String, Vec3, Vec3)> = Vec::new();
    for (entity, mut timer) in &mut timers {
        timer.delay_secs -= dt;
        if timer.delay_secs <= 0.0 {
            ready.push((entity, timer.target_ceg.clone(), timer.pos, timer.dir));
        }
    }
    for (entity, target, pos, dir) in ready {
        commands.entity(entity).despawn();
        spawn_ceg(
            &target,
            pos,
            dir,
            &registry,
            &mut rng,
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut model_cache,
            &mut particle_mesh,
        );
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

/// Evaluate `base + uniform(0, 1) * spread` — upstream's *exact*
/// convention per `CSimpleParticleSystem::Init`:
///
/// ```text
/// p.size = particleSize + guRNG.NextFloat() * particleSizeSpread;
/// p.decayrate = 1.0 / (particleLife + guRNG.NextFloat() * particleLifeSpread);
/// ```
///
/// I had this as `base + signed[-1, 1] * spread * 0.5` — same mean,
/// but a *symmetric* range. For `oldskool`'s squarecloud with
/// `particleSize=14 spread=10` the symmetric form produced sizes in
/// `[9, 19]`; upstream gives `[14, 24]` — so my particles averaged
/// ~14 elmos instead of ~19, visibly smaller. The same regression on
/// `particleLife=12 spread=24` meant particles lived 0–24 frames
/// (mean 12) instead of 12–36 (mean 24) — the impact cloud dissolved
/// twice as fast as authored. Fixed by matching upstream's one-sided
/// range verbatim.
fn eval_with_spread(base: &CegExpr, spread: &CegExpr, rng: &mut u32, mut ctx: EvalCtx) -> f32 {
    ctx.rand01 = next_unit(rng);
    let b = base.eval(&ctx);
    ctx.rand01 = next_unit(rng);
    let s = spread.eval(&ctx);
    b + next_unit(rng) * s
}

fn perpendicular_to(dir: Vec3, rand: f32) -> Vec3 {
    let ref_axis = if dir.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
    let tangent = dir.cross(ref_axis).normalize_or(Vec3::X);
    let angle = rand * std::f32::consts::PI;
    Quat::from_axis_angle(dir.normalize_or(Vec3::Y), angle) * tangent
}

fn sample_stops(stops: &[[f32; 4]], t: f32) -> [f32; 4] {
    match stops.len() {
        0 => [1.0, 1.0, 1.0, 1.0],
        1 => stops[0],
        n => {
            let t = t.clamp(0.0, 1.0);
            let segs = n - 1;
            let scaled = t * segs as f32;
            let idx = (scaled as usize).min(segs - 1);
            let local = scaled - idx as f32;
            let a = stops[idx];
            let b = stops[idx + 1];
            [
                a[0] + (b[0] - a[0]) * local,
                a[1] + (b[1] - a[1]) * local,
                a[2] + (b[2] - a[2]) * local,
                a[3] + (b[3] - a[3]) * local,
            ]
        }
    }
}

fn next_signed(state: &mut u32) -> f32 {
    next_unit(state) * 2.0 - 1.0
}

fn next_unit(state: &mut u32) -> f32 {
    if *state == 0 {
        *state = 0xA3C59AC3;
    }
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    (x as f32 / u32::MAX as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_texture_known_aliases() {
        assert_eq!(
            CegRegistry::resolve_texture("circle"),
            Some("whitecircle.tga")
        );
        assert_eq!(
            CegRegistry::resolve_texture("square"),
            Some("solidwhite.tga")
        );
        assert_eq!(
            CegRegistry::resolve_texture("hline"),
            Some("horizontalline.tga")
        );
        assert_eq!(
            CegRegistry::resolve_texture("bytelaser"),
            Some("bytemegabeam.tga")
        );
        assert_eq!(
            CegRegistry::resolve_texture("pointertrail"),
            Some("pointershottrail.tga")
        );
    }

    #[test]
    fn resolve_texture_case_insensitive_and_trim() {
        assert_eq!(
            CegRegistry::resolve_texture("CIRCLE"),
            Some("whitecircle.tga")
        );
        assert_eq!(
            CegRegistry::resolve_texture("  square  "),
            Some("solidwhite.tga")
        );
    }

    #[test]
    fn resolve_texture_none_and_empty_skip() {
        assert_eq!(CegRegistry::resolve_texture("none"), None);
        assert_eq!(CegRegistry::resolve_texture(""), None);
        assert_eq!(CegRegistry::resolve_texture("nonsense"), None);
    }

    #[test]
    fn rng_next_unit_range() {
        let mut s = 0xDEADBEEF;
        for _ in 0..1000 {
            let v = next_unit(&mut s);
            assert!((0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn rng_next_signed_range() {
        let mut s = 0xA5A5A5A5;
        for _ in 0..1000 {
            let v = next_signed(&mut s);
            assert!((-1.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn rng_zero_seed_recovers() {
        let mut s = 0;
        // First call must produce a non-zero state; the returned value
        // then depends on the auto-seeded state (`0xA3C59AC3`).
        let _ = next_unit(&mut s);
        assert_ne!(s, 0);
    }

    #[test]
    fn sample_stops_empty_is_white() {
        assert_eq!(sample_stops(&[], 0.5), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn sample_stops_single_returns_that_stop() {
        let stops = vec![[0.1, 0.2, 0.3, 0.4]];
        assert_eq!(sample_stops(&stops, 0.7), [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn sample_stops_interpolates_across_segments() {
        let stops = vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
        ];
        // t=0.5 → exactly on stop[1].
        let mid = sample_stops(&stops, 0.5);
        assert!((mid[0] - 0.0).abs() < 1e-4);
        assert!((mid[1] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn eval_with_spread_falls_back_when_spread_is_zero() {
        let base = CegExpr::parse("10");
        let spread = CegExpr::parse("0");
        let mut rng = 1234u32;
        let v = eval_with_spread(&base, &spread, &mut rng, EvalCtx::default());
        assert_eq!(v, 10.0);
    }

    #[test]
    fn eval_with_spread_matches_upstream_one_sided_range() {
        // Upstream's `particleSize + rand(0, 1) * spread` puts the
        // result in `[base, base + spread)`. base=100 spread=40 →
        // result in [100, 140). The old symmetric impl gave [80, 120]
        // instead, shaving 20% off the average and 40% off the upper
        // tail — the root of the "explosions are way too small" bug.
        let base = CegExpr::parse("100");
        let spread = CegExpr::parse("40");
        let mut rng = 0xFEED_FACEu32;
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        let mut sum = 0.0;
        const N: usize = 4000;
        for _ in 0..N {
            let v = eval_with_spread(&base, &spread, &mut rng, EvalCtx::default());
            assert!((100.0..=140.0).contains(&v), "out of range: {v}");
            min = min.min(v);
            max = max.max(v);
            sum += v;
        }
        let mean = sum / N as f32;
        // Mean should be ~120 (midpoint of [100, 140)) and min/max
        // must hug the bounds within a handful of buckets.
        assert!((mean - 120.0).abs() < 2.0, "mean = {mean}, expected ~120");
        assert!(min < 102.0, "min = {min}, expected near 100");
        assert!(max > 138.0, "max = {max}, expected near 140");
    }
}
