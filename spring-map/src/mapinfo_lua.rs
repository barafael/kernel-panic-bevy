//! Parser for Spring's modern `mapinfo.lua` map metadata file.
//!
//! Legacy maps ship a `.smd` TDF file (see [`crate::smd_parser`]). Newer maps
//! like Hex Farm 8 use `mapinfo.lua`, which returns a table describing the
//! map. We run the script in a sandboxed Lua VM with minimal `VFS` stubs and
//! extract the fields we need — start positions, description, gravity, and
//! the atmosphere/lighting data shared with `.smd` output.

use mlua::prelude::*;
use thiserror::Error;

use crate::smd_parser::{Atmosphere, Lighting, MapInfo, StartPosition};

#[derive(Debug, Error)]
pub enum MapInfoLuaError {
    #[error("lua error while loading mapinfo.lua: {0}")]
    Lua(#[from] LuaError),
    #[error("mapinfo.lua did not return a table")]
    NotATable,
}

/// Parse a `mapinfo.lua` script into a [`MapInfo`].
pub fn parse_mapinfo_lua(source: &str) -> Result<MapInfo, MapInfoLuaError> {
    let lua = Lua::new();

    // Spring's stock mapinfo.lua calls `VFS.DirList` / `VFS.Include` to merge
    // overrides from `mapconfig/mapinfo/*.lua`. We don't support overrides, so
    // return empty tables — `ipairs` over an empty table is a safe no-op.
    let vfs = lua.create_table()?;
    vfs.set(
        "DirList",
        lua.create_function(|lua, _: LuaMultiValue| lua.create_table())?,
    )?;
    vfs.set(
        "Include",
        lua.create_function(|lua, _: LuaMultiValue| lua.create_table())?,
    )?;
    lua.globals().set("VFS", vfs)?;

    let result: LuaValue = lua.load(source).call(())?;
    let table = match result {
        LuaValue::Table(t) => t,
        _ => return Err(MapInfoLuaError::NotATable),
    };

    let mut info = MapInfo::default();

    if let Ok(description) = table.get::<String>("description") {
        info.description = description;
    }
    if let Ok(gravity) = table.get::<f32>("gravity") {
        info.gravity = gravity;
    }

    if let Ok(teams) = table.get::<LuaTable>("teams") {
        info.start_positions = extract_start_positions(&teams)?;
    }

    if let Ok(atm) = table.get::<LuaTable>("atmosphere") {
        read_atmosphere(&atm, &mut info.atmosphere);
    }

    if let Ok(lighting) = table.get::<LuaTable>("lighting") {
        read_lighting(&lighting, &mut info.lighting);
    }

    Ok(info)
}

fn extract_start_positions(teams: &LuaTable) -> Result<Vec<StartPosition>, LuaError> {
    let mut out = Vec::new();
    for pair in teams.clone().pairs::<LuaValue, LuaTable>() {
        let (key, team_table) = pair?;
        let team_id: u32 = match key {
            LuaValue::Integer(i) if i >= 0 => i as u32,
            LuaValue::Number(n) if n >= 0.0 => n as u32,
            _ => continue,
        };
        // Stock mapinfo.lua runs `lowerkeys(mapinfo)` which recursively
        // lowercases string keys, so `startPos` may arrive as `startpos`.
        let start_pos = match team_table.get::<LuaTable>("startPos") {
            Ok(t) => t,
            Err(_) => match team_table.get::<LuaTable>("startpos") {
                Ok(t) => t,
                Err(_) => continue,
            },
        };
        let x: f32 = start_pos.get("x").unwrap_or(0.0);
        let z: f32 = start_pos.get("z").unwrap_or(0.0);
        out.push(StartPosition {
            team: team_id,
            x,
            z,
        });
    }
    // Stable order by team id.
    out.sort_by_key(|sp| sp.team);
    Ok(out)
}

/// Get a value by key, falling back to the lowercased key since stock
/// mapinfo.lua runs `lowerkeys(mapinfo)` over the whole table.
fn get_case_insensitive<V: FromLua>(table: &LuaTable, key: &str) -> Option<V> {
    table
        .get::<V>(key)
        .ok()
        .or_else(|| table.get::<V>(key.to_ascii_lowercase().as_str()).ok())
}

fn read_color3(table: &LuaTable, key: &str) -> Option<[f32; 3]> {
    let inner: LuaTable = get_case_insensitive(table, key)?;
    Some([
        inner.get(1).unwrap_or(0.0),
        inner.get(2).unwrap_or(0.0),
        inner.get(3).unwrap_or(0.0),
    ])
}

fn read_atmosphere(table: &LuaTable, out: &mut Atmosphere) {
    if let Some(c) = read_color3(table, "fogColor") {
        out.fog_color = c;
    }
    if let Some(v) = get_case_insensitive::<f32>(table, "fogStart") {
        out.fog_start = v;
    }
    if let Some(c) = read_color3(table, "skyColor") {
        out.sky_color = c;
    }
    if let Some(c) = read_color3(table, "sunColor") {
        out.sun_color = c;
    }
    if let Some(v) = get_case_insensitive::<f32>(table, "cloudDensity") {
        out.cloud_density = v;
    }
}

fn read_lighting(table: &LuaTable, out: &mut Lighting) {
    if let Some(c) = read_color3(table, "sunDir") {
        out.sun_dir = c;
    }
    if let Some(c) = read_color3(table, "groundAmbientColor") {
        out.ground_ambient = c;
    }
    if let Some(c) = read_color3(table, "groundDiffuseColor") {
        out.ground_sun_color = c;
    }
    if let Some(v) = get_case_insensitive::<f32>(table, "groundShadowDensity") {
        out.ground_shadow_density = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_MAPINFO: &str = r#"
        return {
            name = "Test",
            description = "A test map",
            gravity = 75,
            teams = {
                [0] = { startPos = { x = 100, z = 200 } },
                [1] = { startPos = { x = 300, z = 400 } },
            },
            atmosphere = {
                fogColor = {0, 0, 0},
                fogStart = 0.5,
                skyColor = {0.1, 0.2, 0.3},
                sunColor = {1, 1, 1},
                cloudDensity = 0.25,
            },
            lighting = {
                sunDir = {0, 1, 0},
                groundAmbientColor = {0.4, 0.4, 0.4},
                groundDiffuseColor = {0.6, 0.6, 0.6},
                groundShadowDensity = 0.8,
            },
        }
    "#;

    #[test]
    fn parse_minimal_mapinfo() {
        let info = parse_mapinfo_lua(MINIMAL_MAPINFO).unwrap();
        assert_eq!(info.description, "A test map");
        assert!((info.gravity - 75.0).abs() < 0.1);
        assert_eq!(info.start_positions.len(), 2);
        assert_eq!(info.start_positions[0].team, 0);
        assert!((info.start_positions[0].x - 100.0).abs() < 0.1);
        assert!((info.start_positions[1].z - 400.0).abs() < 0.1);
        assert!((info.atmosphere.fog_start - 0.5).abs() < 0.01);
        assert!((info.atmosphere.sky_color[2] - 0.3).abs() < 0.01);
        assert!((info.lighting.ground_shadow_density - 0.8).abs() < 0.01);
    }

    #[test]
    fn rejects_non_table_return() {
        let err = parse_mapinfo_lua("return 42").unwrap_err();
        assert!(matches!(err, MapInfoLuaError::NotATable));
    }

    #[test]
    fn vfs_stubs_allow_stock_mapinfo() {
        // Stock mapinfo.lua calls VFS.DirList; make sure our stubs let it through.
        let source = r#"
            local files = VFS.DirList("mapconfig/mapinfo/", "*.lua")
            for _, f in ipairs(files) do
                VFS.Include(f)
            end
            return { description = "ok", teams = { [0] = { startPos = { x = 1, z = 2 } } } }
        "#;
        let info = parse_mapinfo_lua(source).unwrap();
        assert_eq!(info.description, "ok");
    }
}
