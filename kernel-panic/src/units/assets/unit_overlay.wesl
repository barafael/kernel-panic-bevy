// A unit piece drawn again with a gadget's texture (`gl.Unit(u, true)`
// under `gl.TexGen(..., GL.OBJECT_LINEAR)`): the texture coordinates are
// planes in the piece's own coordinates, the colour multiplies the
// texel (fixed-function `GL_MODULATE`). See `unit_overlay.rs`.
import bevy_pbr::render::mesh_functions::{get_world_from_local, mesh_position_local_to_clip};

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> color: vec4<f32>;
// `GL_OBJECT_PLANE` of the S and T coordinates: (a, b, c, d) gives
// a*x + b*y + c*z + d.
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var<uniform> s_plane: vec4<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(2) var<uniform> t_plane: vec4<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(3) var overlay_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(4) var overlay_sampler: sampler;
// x: the `PolygonOffset` stand-in — the depth is scaled by `1 + x`,
// which (reverse Z) moves the surface that fraction toward the eye.
@group(constants::MATERIAL_BIND_GROUP) @binding(5) var<uniform> params: vec4<f32>;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) local_position: vec3<f32>,
};

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = mesh_position_local_to_clip(
        get_world_from_local(vertex.instance_index),
        vec4<f32>(vertex.position, 1.0),
    );
    out.clip_position.z = out.clip_position.z * (1.0 + params.x);
    out.local_position = vertex.position;
    return out;
}

struct FragmentInput {
    @location(0) local_position: vec3<f32>,
};

@fragment
fn fragment(input: FragmentInput) -> @location(0) vec4<f32> {
    let p = vec4<f32>(input.local_position, 1.0);
    let uv = vec2<f32>(dot(s_plane, p), dot(t_plane, p));
    return color * textureSample(overlay_texture, overlay_sampler, uv);
}
