//! Draw a Lua-composited map (Hex Farm) the way its unsynced gadget
//! does. Hex Farm's SMT is fully transparent and hidden by `voidGround`;
//! everything visible is `HexFarm8.lua`'s immediate-mode geometry over a
//! black void, textured from one 8-region skin atlas:
//!
//! ```text
//!  ________________________________
//!  |/  |  \|   |   |/  |  \|   |   |
//!  ||1 | 2|| 3 | 4 ||5 | 6|| 7 | 8 |
//!  || geo ||   |   ||  |  ||   |   |
//!  |\__|__/|___|___|\__|__/|___|___|
//! ```
//!
//! - Towers (`DrawHex`, gadget l.2181-2370): the top hexagon as two
//!   trapezoids UV-mapped onto regions 1+2 (tower with a datavent) or
//!   5+6, and six walls from the top down to `-VisualPitDepth`,
//!   alternating regions 4/3 (skin 9 has neither `BridgeSupport` nor
//!   `SeparateTip`). Walls fade from `TopColor` to black at the bottom.
//! - Bridges (`DrawRect`, l.2375-2520): a sloped top quad on region 7
//!   and two side skirts `width/3` deep on region 8.
//!
//! The quads are emitted exactly as the gadget's `GL_QUADS` (same
//! vertex order, UVs and colours), split into the two triangles GL
//! would draw. Material is unlit: the gadget draws with lighting off.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use spring_map::LuaCompositing;
use spring_map::lua_layout::HexFarmLayout;
use spring_map::lua_skin::SkinAtlas;

/// `local VisualPitDepth=1024` (gadget l.1972; 2048 only for the
/// skybox-and-fog skins, which KP's skin 9 "Digital" isn't). Tower walls
/// run from the top down to this Y.
pub const VISUAL_PIT_DEPTH: f32 = 1024.0;

/// The atlas regions are 1/8 of the texture wide. `GetLeft(n)` from the
/// gadget: regions 1 and 5 are inset to where the drawn hexagon's left
/// corner sits in the image.
fn get_left(n: u32) -> f32 {
    match n {
        1 => 0.133_974_6 / 8.0,
        5 => 4.133_974_6 / 8.0,
        _ => (n as f32 - 1.0) / 8.0,
    }
}

/// `GetRight(n)` from the gadget — mirror of [`get_left`].
fn get_right(n: u32) -> f32 {
    match n {
        2 => 1.866_025_4 / 8.0,
        6 => 5.866_025_4 / 8.0,
        _ => n as f32 / 8.0,
    }
}

/// `gl.Color` values, RGBA.
pub type Rgba = [f32; 4];
pub const WHITE: Rgba = [1.0, 1.0, 1.0, 1.0];
/// `BottomColor` for skins without `UseSkyboxAndFog`: walls fade to
/// black so the towers dissolve into the void.
pub const BLACK: Rgba = [0.0, 0.0, 0.0, 1.0];

fn scale(c: Rgba, k: f32) -> Rgba {
    [c[0] * k, c[1] * k, c[2] * k, c[3]]
}

/// Growable triangle soup in the gadget's `GL_QUADS` vocabulary.
#[derive(Default)]
pub struct QuadBuffer {
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl QuadBuffer {
    /// One `GL_QUADS` quad: GL splits `(a,b,c,d)` into `(a,b,c)` and
    /// `(a,c,d)`, which also fixes how the UVs interpolate.
    fn quad(&mut self, v: [([f32; 3], [f32; 2], Rgba); 4]) {
        let base = self.positions.len() as u32;
        for (p, uv, c) in v {
            self.positions.push(p);
            self.uvs.push(uv);
            self.colors.push(c);
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn into_mesh(self) -> Mesh {
        // Unlit material: normals only need to exist.
        let normals = vec![[0.0, 1.0, 0.0]; self.positions.len()];
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        mesh.insert_indices(Indices::U32(self.indices));
        mesh
    }
}

/// Everything `DrawHex` needs about one tower.
pub struct HexDraw<'a> {
    /// The six top corners, `c[1]..c[6]` in gadget order.
    pub corners: &'a [[f32; 3]; 6],
    /// Tower top height (`h.y`).
    pub y: f32,
    /// `h.g`: the tower carries a datavent (top texture 1+2, else 5+6).
    pub geo: bool,
    /// `TeamColors[h.owner]`.
    pub top_color: Rgba,
}

/// Emit one tower (`DrawHex`, skin without `BridgeSupport`/`SeparateTip`).
///
/// `side_hex` is the gadget's `sideHex`, the side length of `hex[1]`
/// (all towers share one radius). `progress` animates a rising/sinking
/// tower: the top slides between `-VisualPitDepth` and `y` along
/// `1-(1-p)^2`, and its colour fades in with `p2^4` (l.2208-2225).
pub fn push_hex(
    buf: &mut QuadBuffer,
    h: &HexDraw,
    side_hex: f32,
    team_colored: bool,
    progress: Option<f32>,
) {
    let c = h.corners;
    let mut top = h.top_color;
    let mut bottom = BLACK;
    let mut y = h.y;
    if let Some(progress) = progress {
        if team_colored {
            let p = progress * progress;
            top = scale(top, p);
            bottom = scale(bottom, p);
        } else {
            let p2 = 1.0 - (1.0 - progress) * (1.0 - progress);
            y = y * p2 - VISUAL_PIT_DEPTH * (1.0 - p2);
            top = scale(top, p2.powi(4));
        }
    }
    let v_depth = (y + VISUAL_PIT_DEPTH) / (2.0 * side_hex);
    let at = |k: usize, y: f32| [c[k][0], y, c[k][2]];

    // Top south face: slice 1 if geo, 5 if not.
    let u = if h.geo { 1 } else { 5 };
    buf.quad([
        (at(0, y), [get_right(u), 1.0], top),
        (at(1, y), [get_left(u), 0.75], top),
        (at(2, y), [get_left(u), 0.25], top),
        (at(3, y), [get_right(u), 0.0], top),
    ]);
    // Top north face: slice 2 if geo, 6 if not.
    let u = if h.geo { 2 } else { 6 };
    buf.quad([
        (at(3, y), [get_left(u), 0.0], top),
        (at(4, y), [get_right(u), 0.25], top),
        (at(5, y), [get_right(u), 0.75], top),
        (at(0, y), [get_left(u), 1.0], top),
    ]);
    // Walls, south-east first, alternating wall textures 4 and 3.
    let bot = -VISUAL_PIT_DEPTH;
    for s in 0..6 {
        let n = (s + 1) % 6;
        let u = if s % 2 == 0 { 4 } else { 3 };
        buf.quad([
            (at(s, y), [get_right(u), 0.0], top),
            (at(s, bot), [get_right(u), v_depth], bottom),
            (at(n, bot), [get_left(u), v_depth], bottom),
            (at(n, y), [get_left(u), 0.0], top),
        ]);
    }
}

/// Emit one bridge (`DrawRect`). `corners` are `r.c[1..4]` with their
/// own heights (c1/c4 at the first tower's height, c2/c3 at the
/// second's); `c1`/`c2` are the owner colours of the two towers.
/// `anim` = `(progress, corner)` swings the bridge up from vertical
/// around its far end (the "rotate" animation, l.2415-2475).
pub fn push_rect(
    buf: &mut QuadBuffer,
    corners: &[[f32; 3]; 4],
    mut c1: Rgba,
    mut c2: Rgba,
    team_colored: bool,
    anim: Option<(f32, u8)>,
) {
    let [mut p1, mut p2, mut p3, mut p4] = *corners;
    let dist = |a: [f32; 3], b: [f32; 3]| {
        ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt()
    };
    let void_side = dist(p1, p2);
    let tower_side = dist(p2, p3);
    let thickness = tower_side / 3.0;
    let v = void_side / (4.0 * tower_side);
    let v14 = 0.5 - v;
    let v23 = 0.5 + v;
    let u = get_left(8) + (get_right(8) - get_left(8)) * thickness / tower_side;
    let down = |p: [f32; 3]| [p[0], p[1] - thickness, p[2]];

    if let Some((progress, corner)) = anim {
        if team_colored {
            let p = progress * progress;
            c1 = scale(c1, p);
            c2 = scale(c2, p);
        } else {
            let side_rect = ((p2[0] - p1[0]).powi(2) + (p2[2] - p1[2]).powi(2)).sqrt();
            let sinus = ((1.0 - progress) * std::f32::consts::FRAC_PI_2).sin();
            let cosinus = ((1.0 - progress) * std::f32::consts::FRAC_PI_2).cos();
            let v2 = v * thickness / void_side;
            // Lerp `a` toward the hinge `b` by the swing, dropping it
            // below the hinge by the swung length.
            let swing = |a: [f32; 3], b: [f32; 3]| {
                [
                    a[0] * cosinus + b[0] * (1.0 - cosinus),
                    a[1] - side_rect * sinus,
                    a[2] * cosinus + b[2] * (1.0 - cosinus),
                ]
            };
            if corner == 1 {
                p1 = swing(p1, p2);
                p4 = swing(p4, p3);
                // 14 end cover face.
                buf.quad([
                    (p1, [get_right(7), 0.5 + v], c1),
                    (down(p1), [get_right(7), 0.5 + v + v2], c1),
                    (down(p4), [get_left(7), 0.5 + v + v2], c1),
                    (p4, [get_left(7), 0.5 + v], c1),
                ]);
            } else if corner == 3 {
                p2 = swing(p2, p1);
                p3 = swing(p3, p4);
                // 23 end cover face.
                buf.quad([
                    (p2, [get_right(7), 0.5 + v], c2),
                    (down(p2), [get_right(7), 0.5 + v + v2], c2),
                    (down(p3), [get_left(7), 0.5 + v + v2], c2),
                    (p3, [get_left(7), 0.5 + v], c2),
                ]);
            }
        }
    }

    // Top.
    buf.quad([
        (p1, [get_right(7), v14], c1),
        (p2, [get_right(7), v23], c2),
        (p3, [get_left(7), v23], c2),
        (p4, [get_left(7), v14], c1),
    ]);
    // South side.
    buf.quad([
        (p1, [get_left(8), v14], c1),
        (p2, [get_left(8), v23], c2),
        (down(p2), [u, v23], c2),
        (down(p1), [u, v14], c1),
    ]);
    // North side.
    buf.quad([
        (p3, [get_left(8), v23], c2),
        (p4, [get_left(8), v14], c1),
        (down(p4), [u, v14], c1),
        (down(p3), [u, v23], c2),
    ]);
}

/// Upload the skin atlas with a full mip chain. Both axes repeat, as the
/// gadget's texture does (wall V runs past 1 on tall towers; bridge V
/// can run below 0 on long bridges).
pub fn upload_atlas(atlas: &SkinAtlas, images: &mut Assets<Image>) -> Handle<Image> {
    let (pixels, levels) = super::mipmap::generate_mipmaps_rgba8(
        &atlas.pixels,
        atlas.width as usize,
        atlas.height as usize,
    );
    let mut image = Image::new_uninit(
        Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.data = Some(pixels);
    image.texture_descriptor.mip_level_count = levels;
    // The gadget loads it with `:a:` (anisotropic filtering).
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        min_filter: ImageFilterMode::Linear,
        mag_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 16,
        ..default()
    });
    images.add(image)
}

/// The atlas material: unlit (the gadget draws with lighting off),
/// texture × vertex colour, double-sided (no culling in the gadget).
pub fn atlas_material(
    atlas: Handle<Image>,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color_texture: Some(atlas),
        unlit: true,
        cull_mode: None,
        ..default()
    })
}

/// Spawn the layout's visible towers and bridges as one static mesh.
/// (Datavents come in as regular map features — see `spawn_map_world`.)
pub fn spawn_lua_compositing(
    compositing: &LuaCompositing,
    layout: &HexFarmLayout,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) {
    let Some(first) = layout.hexes.first() else {
        return;
    };
    let material = atlas_material(upload_atlas(&compositing.atlas, images), materials);
    let side_hex = ((first.corners[1][0] - first.corners[0][0]).powi(2)
        + (first.corners[1][2] - first.corners[0][2]).powi(2))
    .sqrt();

    let mut buf = QuadBuffer::default();
    for hex in layout.hexes.iter().filter(|h| !h.hidden) {
        let draw = HexDraw {
            corners: &hex.corners,
            y: hex.center[1],
            geo: hex.g != 0,
            top_color: WHITE,
        };
        push_hex(&mut buf, &draw, side_hex, layout.team_colored, None);
    }
    for rect in layout.bridges.iter().filter(|r| !r.hidden) {
        push_rect(&mut buf, &rect.corners, WHITE, WHITE, layout.team_colored, None);
    }
    if !buf.is_empty() {
        commands.spawn((Mesh3d(meshes.add(buf.into_mesh())), MeshMaterial3d(material)));
    }
}

/// Even-odd point-in-polygon test (the gadget's `IsInsidePolygon`).
fn inside(poly: &[[f32; 3]], x: f32, z: f32) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if z >= a[2].min(b[2])
            && z < a[2].max(b[2])
            && x < (b[0] - a[0]) * (z - a[2]) / (b[2] - a[2]) + a[0]
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Minimap fallback for a voidGround map: there is no ground texture,
/// so paint the visible towers and bridges' footprints over black.
pub fn minimap_pixels(layout: &HexFarmLayout, world: Vec2, size: usize) -> Vec<u8> {
    const TOWER: [u8; 4] = [150, 24, 24, 255];
    const VENT: [u8; 4] = [40, 150, 40, 255];
    const BRIDGE: [u8; 4] = [95, 40, 40, 255];
    let mut px = vec![0u8; size * size * 4];
    for (i, chunk) in px.chunks_exact_mut(4).enumerate() {
        chunk[3] = 255;
        let x = ((i % size) as f32 + 0.5) / size as f32 * world.x;
        let z = ((i / size) as f32 + 0.5) / size as f32 * world.y;
        let hex = layout
            .hexes
            .iter()
            .find(|h| !h.hidden && inside(&h.corners, x, z));
        let color = if let Some(h) = hex {
            let r = ((x - h.center[0]).powi(2) + (z - h.center[2]).powi(2)).sqrt();
            if h.g != 0 && r < 64.0 { Some(VENT) } else { Some(TOWER) }
        } else if layout
            .bridges
            .iter()
            .any(|r| !r.hidden && inside(&r.corners, x, z))
        {
            Some(BRIDGE)
        } else {
            None
        };
        if let Some(c) = color {
            chunk.copy_from_slice(&c);
        }
    }
    px
}
