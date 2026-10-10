//! The map's terrain types as a ground-speed multiplier.
//!
//! A Spring map paints a terrain type into every 2×2 squares (the SMF
//! typemap), and the map info gives each type move-speed multipliers
//! per speed-mod class. `CMoveMath::GetPosSpeedMod` multiplies the
//! slope's speed modifier by the square's type speed — for pathing (the
//! QTPFS node costs) and for driving (`CGroundMoveType::ChangeSpeed`)
//! alike. Eight of the shipped maps paint with it: Direct Memory
//! Access's "SDRAM" lanes run 4×, its "EEPROM" 0.25×, and its "ROM"
//! (also on Central Hub) closes the ground entirely; Corrupted Core,
//! Dual / Quad Core and Digital Divide have 2× lanes, Spooler Buffer a
//! slow strip.
//!
//! Every Kernel Panic move class (`MOVEINFO.TDF`: LIGHT, MEDIUM, HEAVY)
//! is a `KBot` by the engine's name rule (`ParseSpeedModClass`: no
//! "tank" / "hover" / "ship" / "boat" in the name), so the kbot speeds
//! are the ones that apply.

use bevy::math::Vec2;
use spring_map::map_types::{SQUARE_SIZE, TerrainTypes};
use spring_pathfinding::SpeedMap;

/// Kbot speed multiplier of every typemap cell (2×2 squares).
#[derive(Debug, Clone, Default)]
pub struct TerrainSpeedMap {
    /// Cells per row (`mapDims.hmapx`); 0 = neutral everywhere.
    width: usize,
    height: usize,
    speeds: Vec<f32>,
}

impl TerrainSpeedMap {
    /// From the SMF typemap and the map's terrain types. A map without
    /// a typemap, or whose types are all neutral, needs no lookups.
    pub fn new(typemap: &[u8], width: usize, height: usize, types: &TerrainTypes) -> Self {
        if types.is_neutral() || typemap.len() != width * height || width == 0 {
            return Self::default();
        }
        Self {
            width,
            height,
            speeds: typemap.iter().map(|&t| types.get(t).kbot).collect(),
        }
    }

    /// Does any square run at other than full speed?
    pub fn is_neutral(&self) -> bool {
        self.speeds.is_empty()
    }

    /// The multiplier of heightmap square `(x, z)`.
    pub fn at_square(&self, x: u32, z: u32) -> f32 {
        if self.speeds.is_empty() {
            return 1.0;
        }
        let (cx, cz) = ((x / 2) as usize, (z / 2) as usize);
        if cx >= self.width || cz >= self.height {
            return 1.0;
        }
        self.speeds[cz * self.width + cx]
    }

    /// The multiplier under world position `pos` (x, z).
    pub fn at(&self, pos: Vec2) -> f32 {
        if pos.x < 0.0 || pos.y < 0.0 {
            return 1.0;
        }
        let sq = SQUARE_SIZE as f32;
        self.at_square((pos.x / sq) as u32, (pos.y / sq) as u32)
    }

    /// Fold the multipliers into a per-square slope speed map (the
    /// pathfinder's `GetPosSpeedMod`). QTPFS clamps the result to its
    /// `[0, 2]` speed-mod range, as upstream does.
    pub fn apply(&self, speed_map: &mut SpeedMap) {
        if self.is_neutral() {
            return;
        }
        for z in 0..speed_map.height {
            for x in 0..speed_map.width {
                let i = (z * speed_map.width + x) as usize;
                speed_map.speeds[i] *= self.at_square(x, z);
            }
        }
        speed_map.refresh_max_speed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types() -> TerrainTypes {
        let mut types = TerrainTypes::default();
        types.move_speeds[0].kbot = 0.0;
        types.move_speeds[1].kbot = 0.25;
        types.move_speeds[3].kbot = 4.0;
        types.move_speeds[3].tank = 9.0; // not a KP class
        types
    }

    /// Each typemap cell covers 2×2 squares; the kbot speed applies.
    #[test]
    fn cells_cover_two_by_two_squares() {
        // 2×2 cells → a 4×4-square map.
        let map = TerrainSpeedMap::new(&[0, 1, 2, 3], 2, 2, &types());
        assert_eq!(map.at_square(0, 0), 0.0);
        assert_eq!(map.at_square(1, 1), 0.0);
        assert_eq!(map.at_square(2, 0), 0.25);
        assert_eq!(map.at_square(0, 3), 1.0);
        assert_eq!(map.at_square(3, 3), 4.0);
        assert_eq!(map.at(Vec2::new(3.5 * 8.0, 2.5 * 8.0)), 4.0);
        assert_eq!(map.at_square(9, 9), 1.0, "off the map: neutral");
    }

    #[test]
    fn folds_into_the_speed_map() {
        let terrain = TerrainSpeedMap::new(&[0, 1, 2, 3], 2, 2, &types());
        let mut speed_map = SpeedMap::uniform(4, 4, 0.5);
        terrain.apply(&mut speed_map);
        assert_eq!(speed_map.get(0, 0), 0.0, "ROM closes the square");
        assert_eq!(speed_map.get(2, 1), 0.125);
        assert_eq!(speed_map.get(3, 3), 2.0);
        assert_eq!(speed_map.max_speed(), 2.0);
    }

    #[test]
    fn neutral_without_typemap_or_types() {
        assert!(TerrainSpeedMap::new(&[], 2, 2, &types()).is_neutral());
        assert!(TerrainSpeedMap::new(&[3; 4], 2, 2, &TerrainTypes::default()).is_neutral());
    }
}
