//! Dump computed AABB + ground-lift for models, mirroring the game's
//! `compute_model_aabb` walk. Run from spring-unit-mesh/:
//!   cargo run --release --example aabb_dump -- <objects3d-dir> <name.s3o> ...
//! Set PIECES=1 to also print the per-piece tree with y extents.

use spring_unit_mesh::{parse_s3o, S3OPiece};

fn walk(piece: &S3OPiece, parent: [f32; 3], mins: &mut [f32; 3], maxs: &mut [f32; 3]) {
    let world = [
        parent[0] + piece.offset[0],
        parent[1] + piece.offset[1],
        parent[2] + piece.offset[2],
    ];
    for v in &piece.vertices {
        for a in 0..3 {
            mins[a] = mins[a].min(v.position[a] + world[a]);
            maxs[a] = maxs[a].max(v.position[a] + world[a]);
        }
    }
    for c in &piece.children {
        walk(c, world, mins, maxs);
    }
}

fn dump_pieces(piece: &S3OPiece, parent: [f32; 3], depth: usize) {
    let world = [
        parent[0] + piece.offset[0],
        parent[1] + piece.offset[1],
        parent[2] + piece.offset[2],
    ];
    let (mut mins, mut maxs) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in &piece.vertices {
        for a in 0..3 {
            mins[a] = mins[a].min(v.position[a] + world[a]);
            maxs[a] = maxs[a].max(v.position[a] + world[a]);
        }
    }
    if piece.vertices.is_empty() {
        println!(
            "{}{:.<20} EMPTY offset=({:.1},{:.1},{:.1})",
            "  ".repeat(depth),
            piece.name,
            piece.offset[0],
            piece.offset[1],
            piece.offset[2]
        );
    } else {
        println!(
            "{}{:.<20} y=[{:>6.1} .. {:>6.1}]  x=[{:.1} .. {:.1}]",
            "  ".repeat(depth),
            piece.name,
            mins[1],
            maxs[1],
            mins[0],
            maxs[0]
        );
    }
    for c in &piece.children {
        dump_pieces(c, world, depth + 1);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = &args[0];
    for name in &args[1..] {
        let path = format!("{dir}/{name}");
        let Ok(data) = std::fs::read(&path) else {
            println!("{name}: READ FAILED");
            continue;
        };
        let Ok(model) = parse_s3o(&data) else {
            println!("{name}: PARSE FAILED");
            continue;
        };
        let mut mins = [f32::MAX; 3];
        let mut maxs = [f32::MIN; 3];
        walk(&model.root_piece, [0.0; 3], &mut mins, &mut maxs);
        if std::env::var("PIECES").is_ok() {
            dump_pieces(&model.root_piece, [0.0; 3], 0);
        }
        let lift = -mins[1];
        let hlift = -model.mins[1];
        println!(
            "{name:<24} walked mins.y={:>7.1}  header mins.y={:>7.1}  game_lift=-header.y={hlift:>6.1}",
            mins[1], model.mins[1],
        );
    }
}
