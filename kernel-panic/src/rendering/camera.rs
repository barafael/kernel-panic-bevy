use bevy::{input::mouse::MouseWheel, prelude::*, render::view::Hdr};

/// Marker component for the main RTS camera.
#[derive(Component)]
pub struct RtsCamera;

/// Persistent camera state tracked across frames.
///
/// `yaw` / `pitch` / `distance` are the *target* values. The actual rendered
/// values live in `smooth_*` fields and interpolate toward the targets each
/// frame, giving the camera a polished, weighty feel.
#[derive(Component)]
pub struct RtsCameraState {
    pub focus: Vec3,
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,

    // Smoothed (rendered) values — chase the targets above.
    smooth_focus: Vec3,
    smooth_distance: f32,
    smooth_yaw: f32,
    smooth_pitch: f32,
}

impl Default for RtsCameraState {
    // Manual impl (not `better_default`): each `smooth_*` field must
    // mirror its non-smoothed counterpart so the camera doesn't lerp
    // from an arbitrary origin on the first frame. `better_default`
    // doesn't let a field default reference another field.
    fn default() -> Self {
        let focus = Vec3::new(1024.0, 0.0, 1024.0);
        let distance = 800.0;
        let yaw = 0.0;
        let pitch = std::f32::consts::FRAC_PI_4;
        Self {
            focus,
            distance,
            yaw,
            pitch,
            smooth_focus: focus,
            smooth_distance: distance,
            smooth_yaw: yaw,
            smooth_pitch: pitch,
        }
    }
}

impl RtsCameraState {
    /// Snap the camera to look at a new focus point (no animation).
    pub fn snap_to(&mut self, focus: Vec3, distance: f32) {
        self.focus = focus;
        self.distance = distance;
        self.smooth_focus = focus;
        self.smooth_distance = distance;
    }
}

/// World-space bounds of the map. The camera focus is clamped to stay
/// within `margin` of these bounds. Set by the map loader at startup.
#[derive(Resource, Default)]
pub struct MapBounds {
    pub min: Vec3,
    pub max: Vec3,
    /// How far beyond the edge the camera can travel (fraction of map size).
    pub margin_fraction: f32,
}

impl MapBounds {
    pub fn from_map_extents(min: Vec3, max: Vec3) -> Self {
        Self {
            min,
            max,
            margin_fraction: 0.25,
        }
    }

    /// Clamp a focus point to stay within the allowed region.
    pub fn clamp_focus(&self, focus: Vec3) -> Vec3 {
        let extent = self.max - self.min;
        let margin = extent * self.margin_fraction;
        Vec3::new(
            focus.x.clamp(self.min.x - margin.x, self.max.x + margin.x),
            focus.y, // Y (height) is not clamped — follows terrain
            focus.z.clamp(self.min.z - margin.z, self.max.z + margin.z),
        )
    }
}

#[derive(Resource, better_default::Default)]
pub struct CameraSettings {
    #[default(800.0)]
    pub pan_speed: f32,
    #[default(0.8)]
    pub rotate_speed_keys: f32,
    #[default(0.003)]
    pub rotate_speed_mouse: f32,
    #[default(150.0)]
    pub min_distance: f32,
    #[default(3000.0)]
    pub max_distance: f32,
    /// Lowest pitch the camera accepts, ~14°.
    #[default(0.25)]
    pub min_pitch: f32,
    /// Highest pitch the camera accepts, ~85° (just below straight-down
    /// so the RTS orbit never flips past vertical).
    #[default(std::f32::consts::FRAC_PI_2 - 0.05)]
    pub max_pitch: f32,
    /// How fast the smooth values chase the targets (higher = snappier).
    /// 10 is responsive, 4 is cinematic.
    #[default(8.0)]
    pub smoothing: f32,
}

pub fn spawn_camera(mut commands: Commands) {
    let state = RtsCameraState::default();
    let transform = compute_transform_from_state(&state);

    commands.spawn((
        RtsCamera,
        state,
        Camera3d::default(),
        // Default Bevy far plane is 1000, which clips large maps long
        // before the map fog takes over. `apply_fog` sizes the fog to
        // the map diagonal, so push the far plane past any sensible map.
        Projection::Perspective(PerspectiveProjection {
            far: 40_000.0,
            ..default()
        }),
        transform,
        Hdr,
        bevy::post_process::bloom::Bloom {
            intensity: 0.15,
            ..default()
        },
        DistanceFog {
            color: Color::BLACK,
            falloff: FogFalloff::Linear {
                start: 3600.0,
                end: 4000.0,
            },
            ..default()
        },
    ));
}

/// Build a `Transform` from the *smoothed* state values.
pub fn compute_transform_from_state(state: &RtsCameraState) -> Transform {
    let horizontal = state.smooth_distance * state.smooth_pitch.cos();
    let vertical = state.smooth_distance * state.smooth_pitch.sin();

    let offset = Vec3::new(
        horizontal * state.smooth_yaw.sin(),
        vertical,
        horizontal * state.smooth_yaw.cos(),
    );

    let eye = state.smooth_focus + offset;
    Transform::from_translation(eye).looking_at(state.smooth_focus, Vec3::Y)
}

/// Forward direction on the XZ ground plane for the current *smooth* yaw.
fn fwd_ground(state: &RtsCameraState) -> Vec3 {
    Vec3::new(-state.smooth_yaw.sin(), 0.0, -state.smooth_yaw.cos())
}

fn right_ground(state: &RtsCameraState) -> Vec3 {
    let f = fwd_ground(state);
    Vec3::new(f.z, 0.0, -f.x)
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn camera_control(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    mut scroll_events: MessageReader<MouseWheel>,
    mut mouse_motion: MessageReader<bevy::input::mouse::MouseMotion>,
    settings: Res<CameraSettings>,
    bounds: Res<MapBounds>,
    mut query: Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
) {
    let Ok((mut state, mut transform)) = query.single_mut() else {
        return;
    };

    let delta_time = time.delta_secs();

    // --- Pan (arrow keys only — WASD reserved for hotkeys) ---
    let fwd = fwd_ground(&state);
    let right = right_ground(&state);

    let mut pan = Vec3::ZERO;
    if keys.pressed(KeyCode::ArrowUp) {
        pan += fwd;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        pan -= fwd;
    }
    if keys.pressed(KeyCode::ArrowLeft) {
        pan += right;
    }
    if keys.pressed(KeyCode::ArrowRight) {
        pan -= right;
    }

    if pan != Vec3::ZERO {
        let speed = settings.pan_speed * (state.distance / 500.0).max(0.3);
        state.focus += pan.normalize() * speed * delta_time;
    }

    state.focus = bounds.clamp_focus(state.focus);

    // --- Zoom (scroll wheel) ---
    // Accumulate all scroll ticks into one factor so a single big scroll
    // can't skip past the minimum distance.
    let mut zoom_ticks: f32 = 0.0;
    for event in scroll_events.read() {
        zoom_ticks += event.y;
    }
    if zoom_ticks != 0.0 {
        let factor = 1.0 - zoom_ticks.clamp(-5.0, 5.0) * 0.06;
        state.distance =
            (state.distance * factor).clamp(settings.min_distance, settings.max_distance);
    }

    // --- Orbit (middle-mouse drag) ---
    if mouse_buttons.pressed(MouseButton::Middle) {
        for motion in mouse_motion.read() {
            state.yaw -= motion.delta.x * settings.rotate_speed_mouse;
            state.pitch = (state.pitch + motion.delta.y * settings.rotate_speed_mouse)
                .clamp(settings.min_pitch, settings.max_pitch);
        }
    } else {
        mouse_motion.clear();
    }

    // --- Q/E rotate shortcuts ---
    if keys.pressed(KeyCode::KeyQ) {
        state.yaw += settings.rotate_speed_keys * delta_time;
    }
    if keys.pressed(KeyCode::KeyE) {
        state.yaw -= settings.rotate_speed_keys * delta_time;
    }

    // --- Smooth interpolation ---
    let t = (settings.smoothing * delta_time).min(1.0);
    state.smooth_focus = state.smooth_focus.lerp(state.focus, t);
    state.smooth_distance = state.smooth_distance.lerp(state.distance, t);
    state.smooth_yaw = state.smooth_yaw.lerp(state.yaw, t);
    state.smooth_pitch = state.smooth_pitch.lerp(state.pitch, t);

    *transform = compute_transform_from_state(&state);
}
