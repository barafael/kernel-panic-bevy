#import bevy_pbr::forward_io::VertexOutput

// Must match TerrainMaterialUniform in terrain_material.rs (std140 layout).
struct TerrainMaterialUniform {
    line_color: vec4<f32>,
    fill_color: vec4<f32>,
    line_width: f32,
    emissive_strength: f32,
    _padding1: f32,
    _padding2: f32,
};

@group(2) @binding(0)
var<storage, read> material: TerrainMaterialUniform;

// Spring heightmap vertices are spaced 8 elmos apart (square_size = 8).
const SQUARE_SIZE: f32 = 8.0;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Derive grid coordinates from world position.
    let grid = in.world_position.xz / SQUARE_SIZE;

    // Distance to nearest integer grid line along each axis.
    let grid_frac = fract(grid);
    let dist_x = min(grid_frac.x, 1.0 - grid_frac.x);
    let dist_z = min(grid_frac.y, 1.0 - grid_frac.y);

    // Screen-space derivative for antialiasing.
    let dx = fwidth(grid.x);
    let dz = fwidth(grid.y);

    // Antialiased line: smoothstep from line edge to line center.
    let line_x = 1.0 - smoothstep(0.0, material.line_width * dx + dx, dist_x);
    let line_z = 1.0 - smoothstep(0.0, material.line_width * dz + dz, dist_z);
    let line = max(line_x, line_z);

    // Mix between fill and line color.
    let base_color = mix(material.fill_color, material.line_color, line);

    // Apply emissive glow to the lines (bloom picks this up).
    let emissive = base_color * material.emissive_strength * line;
    let final_color = base_color + emissive;

    return vec4<f32>(final_color.rgb, 1.0);
}
