/// Execute Lua heightmap gadgets against a parsed map's height array.
///
/// This stubs the minimal Spring API surface needed for heightmap gadgets
/// like Palladium's `PalladiumHeight.lua` to modify the terrain during
/// their `Initialize()` call.
///
/// Map gadgets often run in two halves — synced (gameplay/heightmap)
/// and unsynced (rendering). After the synced `Initialize()` completes
/// we drive the synced→unsynced handshake (`gadget:RecvLuaMsg`) and
/// capture every `SendToUnsynced(...)` call. Callers that mirror the
/// unsynced compositing logic (e.g. selecting a Lua-driven map skin)
/// can read those messages from [`LuaGadgetResult`] without touching
/// the OpenGL API the unsynced half would normally use.
use std::cell::RefCell;
use std::rc::Rc;

use mlua::prelude::*;

use crate::map_types::{LuaFile, ParsedMap, SQUARE_SIZE, UnsyncedArg, UnsyncedMessage};

/// Per-gadget output from running the synced half.
#[derive(Debug, Default, Clone)]
pub struct LuaGadgetResult {
    /// Captured `SendToUnsynced(...)` calls from this gadget, in order.
    pub unsynced_messages: Vec<UnsyncedMessage>,
}

/// Find and execute any heightmap gadgets from the extracted Lua files.
///
/// Modifies `map.heights` in place. Returns one [`LuaGadgetResult`] per
/// gadget that ran (in load order). The number of executed gadgets is
/// `result.len()`.
pub fn apply_lua_heightmap_gadgets(
    map: &mut ParsedMap,
    lua_files: &[LuaFile],
) -> Vec<LuaGadgetResult> {
    let gadgets: Vec<&LuaFile> = lua_files
        .iter()
        .filter(|f| {
            let lower = f.path.to_ascii_lowercase();
            lower.contains("luarules/gadgets") && lower.ends_with(".lua")
        })
        .collect();

    if gadgets.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();

    for gadget in &gadgets {
        // Only execute gadgets that look like they modify the heightmap.
        let lower_source = gadget.content.to_ascii_lowercase();
        if !lower_source.contains("setheightmap") {
            continue;
        }

        match execute_heightmap_gadget(map, &gadget.content) {
            Ok(result) => {
                eprintln!("Executed heightmap gadget: {}", gadget.path);
                results.push(result);
            }
            Err(error) => {
                eprintln!(
                    "Failed to execute heightmap gadget {}: {error}",
                    gadget.path
                );
            }
        }
    }

    results
}

fn execute_heightmap_gadget(
    map: &mut ParsedMap,
    source: &str,
) -> Result<LuaGadgetResult, LuaError> {
    let heightmap_w = map.header.heightmap_width();
    let heightmap_h = map.header.heightmap_height();
    let world_size_x = (map.header.map_x * SQUARE_SIZE) as f32;
    let world_size_z = (map.header.map_y * SQUARE_SIZE) as f32;

    // Shared mutable reference to the heights array via Rc<RefCell>. Taking
    // heights out of `map` avoids a full clone while the gadget runs — but we
    // must always put them back (even on error) so the map stays well-formed.
    let heights = Rc::new(RefCell::new(std::mem::take(&mut map.heights)));
    let unsynced: Rc<RefCell<Vec<UnsyncedMessage>>> = Rc::new(RefCell::new(Vec::new()));

    let result = run_gadget_internal(
        source,
        &heights,
        &unsynced,
        heightmap_w,
        heightmap_h,
        world_size_x,
        world_size_z,
    );

    // Always restore heights, whether the gadget succeeded, partially
    // succeeded, or errored before touching them.
    map.heights = Rc::try_unwrap(heights)
        .expect("all Lua references should be dropped after the VM is gone")
        .into_inner();

    let unsynced_messages = Rc::try_unwrap(unsynced)
        .expect("all Lua references should be dropped after the VM is gone")
        .into_inner();

    result.map(|()| LuaGadgetResult { unsynced_messages })
}

fn run_gadget_internal(
    source: &str,
    heights: &Rc<RefCell<Vec<f32>>>,
    unsynced: &Rc<RefCell<Vec<UnsyncedMessage>>>,
    heightmap_w: usize,
    heightmap_h: usize,
    world_size_x: f32,
    world_size_z: f32,
) -> Result<(), LuaError> {
    let lua = Lua::new();

    // --- Stub the Spring table ---
    let spring_table = lua.create_table()?;

    // Spring.SetHeightMap(x, z, height)
    {
        let heights_ref = Rc::clone(heights);
        let set_height_map = lua.create_function(move |_, (x, z, height): (f32, f32, f32)| {
            let hx = (x / SQUARE_SIZE as f32).round() as usize;
            let hz = (z / SQUARE_SIZE as f32).round() as usize;
            let mut h = heights_ref.borrow_mut();
            if hx < heightmap_w && hz < heightmap_h {
                h[hz * heightmap_w + hx] = height;
            }
            Ok(())
        })?;
        spring_table.set("SetHeightMap", set_height_map)?;
    }

    // Spring.SetHeightMapFunc(func, ...) — calls the function once, forwarding extra args
    {
        let set_height_map_func =
            lua.create_function(|_, (func, args): (LuaFunction, LuaMultiValue)| {
                func.call::<()>(args)?;
                Ok(())
            })?;
        spring_table.set("SetHeightMapFunc", set_height_map_func)?;
    }

    // Spring.GetGroundHeight(x, z)
    {
        let heights_ref = Rc::clone(heights);
        let get_ground_height = lua.create_function(move |_, (x, z): (f32, f32)| {
            let hx = (x / SQUARE_SIZE as f32).round() as usize;
            let hz = (z / SQUARE_SIZE as f32).round() as usize;
            let h = heights_ref.borrow();
            if hx < heightmap_w && hz < heightmap_h {
                Ok(h[hz * heightmap_w + hx])
            } else {
                Ok(0.0)
            }
        })?;
        spring_table.set("GetGroundHeight", get_ground_height)?;
    }

    // Spring.GetMapOptions/GetModOptions — return empty table (defaults)
    spring_table.set(
        "GetMapOptions",
        lua.create_function(|lua, ()| lua.create_table())?,
    )?;
    spring_table.set(
        "GetModOptions",
        lua.create_function(|lua, ()| lua.create_table())?,
    )?;

    // Spring.Echo(msg) — accept any args and stringify the first
    spring_table.set(
        "Echo",
        lua.create_function(|_, args: LuaMultiValue| {
            let mut parts: Vec<String> = Vec::with_capacity(args.len());
            for v in args.iter() {
                parts.push(match v {
                    LuaValue::String(s) => s.to_str()?.to_string(),
                    LuaValue::Integer(i) => i.to_string(),
                    LuaValue::Number(n) => n.to_string(),
                    LuaValue::Boolean(b) => b.to_string(),
                    LuaValue::Nil => "nil".to_string(),
                    other => format!("{other:?}"),
                });
            }
            eprintln!("[Lua] {}", parts.join(" "));
            Ok(())
        })?,
    )?;

    // Boolean stubs
    spring_table.set("IsCheatingEnabled", lua.create_function(|_, ()| Ok(false))?)?;
    spring_table.set("IsDevLuaEnabled", lua.create_function(|_, ()| Ok(false))?)?;
    spring_table.set("IsGodModeEnabled", lua.create_function(|_, ()| Ok(false))?)?;

    // Team/gaia stubs — pretend we have two teams plus gaia
    spring_table.set(
        "GetTeamList",
        lua.create_function(|lua, ()| {
            let t = lua.create_table()?;
            t.push(0)?;
            t.push(1)?;
            t.push(2)?; // gaia
            Ok(t)
        })?,
    )?;
    spring_table.set("GetGaiaTeamID", lua.create_function(|_, ()| Ok(2))?)?;

    // Terrain / metal / smoothmesh setters — no-op (we only care about heights)
    spring_table.set(
        "SetMapSquareTerrainType",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    spring_table.set(
        "SetMetalAmount",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    spring_table.set(
        "SetSmoothMesh",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    spring_table.set(
        "SetSmoothMeshFunc",
        lua.create_function(|_, (func, args): (LuaFunction, LuaMultiValue)| {
            func.call::<()>(args)?;
            Ok(())
        })?,
    )?;

    // Watchdog / config stubs
    spring_table.set(
        "ClearWatchDogTimer",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    spring_table.set(
        "SetConfigInt",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;

    // Stubs for synced→unsynced messaging round-trip and gameframe
    // accessors that some gadgets sprinkle through their synced half.
    spring_table.set(
        "SendLuaRulesMsg",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    spring_table.set(
        "SendCommands",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    spring_table.set("GetGameFrame", lua.create_function(|_, ()| Ok(0))?)?;
    spring_table.set(
        "GetTeamColor",
        lua.create_function(|_, _: LuaMultiValue| Ok((1.0, 1.0, 1.0, 1.0)))?,
    )?;

    // Feature stubs (return empty results / no-op)
    spring_table.set(
        "GetAllFeatures",
        lua.create_function(|lua, ()| lua.create_table())?,
    )?;
    spring_table.set(
        "GetFeaturesInRectangle",
        lua.create_function(|lua, _: LuaMultiValue| lua.create_table())?,
    )?;
    spring_table.set(
        "GetFeaturePosition",
        lua.create_function(|_, _: LuaMultiValue| Ok((0.0, 0.0, 0.0)))?,
    )?;
    spring_table.set(
        "GetFeatureDefID",
        lua.create_function(|_, _: LuaMultiValue| Ok(0))?,
    )?;
    spring_table.set(
        "SetFeaturePosition",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    spring_table.set(
        "CreateFeature",
        lua.create_function(|_, _: LuaMultiValue| Ok(0))?,
    )?;
    spring_table.set(
        "DestroyFeature",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;

    lua.globals().set("Spring", spring_table)?;

    // --- Stub the Game table ---
    let game_table = lua.create_table()?;
    game_table.set("mapSizeX", world_size_x as i32)?;
    game_table.set("mapSizeZ", world_size_z as i32)?;
    // modName drives mod-specific branches in HexFarm — claim we're kernel-panic.
    game_table.set("modName", "Kernel Panic")?;
    game_table.set("startPosType", 0)?; // fixed start positions
    let armor_types = lua.create_table()?;
    armor_types.set("default", 1)?;
    game_table.set("armorTypes", armor_types)?;
    lua.globals().set("Game", game_table)?;

    // --- Stub UnitDefNames / UnitDefs / FeatureDefs / WeaponDefs ---
    // UnitDefNames is accessed like UnitDefNames["kernel"]; HexFarm uses it to
    // pick a skin, and the "kernel" branch is the right one for us.
    let unit_def_names = lua.create_table()?;
    unit_def_names.set("kernel", lua.create_table()?)?;
    lua.globals().set("UnitDefNames", unit_def_names)?;
    lua.globals().set("UnitDefs", lua.create_table()?)?;
    lua.globals().set("FeatureDefs", lua.create_table()?)?;
    lua.globals().set("WeaponDefs", lua.create_table()?)?;

    // --- SendToUnsynced(msgName, ...args) — capture for the renderer ---
    {
        let unsynced_ref = Rc::clone(unsynced);
        let send_to_unsynced = lua.create_function(move |_, args: LuaMultiValue| {
            let msg: UnsyncedMessage = args
                .iter()
                .map(|v| match v {
                    LuaValue::Integer(i) => UnsyncedArg::Integer(*i),
                    LuaValue::Number(n) => UnsyncedArg::Number(*n),
                    LuaValue::String(s) => {
                        UnsyncedArg::String(s.to_str().map(|s| s.to_string()).unwrap_or_default())
                    }
                    LuaValue::Boolean(b) => UnsyncedArg::Bool(*b),
                    LuaValue::Nil => UnsyncedArg::Nil,
                    _ => UnsyncedArg::Nil,
                })
                .collect();
            unsynced_ref.borrow_mut().push(msg);
            Ok(())
        })?;
        lua.globals().set("SendToUnsynced", send_to_unsynced)?;
    }

    // --- Stub gadgetHandler ---
    let gadget_handler = lua.create_table()?;
    gadget_handler.set("IsSyncedCode", lua.create_function(|_, ()| Ok(true))?)?;
    gadget_handler.set(
        "RemoveCallIn",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    gadget_handler.set(
        "RemoveGadget",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    gadget_handler.set(
        "AddSyncAction",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    lua.globals().set("gadgetHandler", gadget_handler)?;

    // --- Stub Script (some gadgets call Script.SetWatchWeapon etc) ---
    let script_table = lua.create_table()?;
    script_table.set(
        "SetWatchWeapon",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    lua.globals().set("Script", script_table)?;

    // --- Load the gadget source ---
    // Spring gadgets use `function gadget:GetInfo()` and `function gadget:Initialize()`.
    // We need to provide a `gadget` table, load the source, then call Initialize.
    let gadget_table = lua.create_table()?;
    lua.globals().set("gadget", &gadget_table)?;

    // Execute the gadget source in the global scope.
    lua.load(source).exec()?;

    // Call gadget:Initialize() if it exists.
    if let Ok(init_fn) = gadget_table.get::<LuaFunction>("Initialize") {
        init_fn.call::<()>(&gadget_table)?;
    }

    // Trigger the synced→unsynced handshake. Spring gadgets that compose
    // visuals from runtime data ship a `RecvLuaMsg(msg, player)` callin
    // that, on a magic ping from the unsynced half, fans out the layout
    // via `SendToUnsynced(...)`. We don't run unsynced, so we ping it
    // directly with the conventional "<gadget name>: send me the data!"
    // string used by HexFarm and similar.
    if let Ok(recv) = gadget_table.get::<LuaFunction>("RecvLuaMsg") {
        if let Ok(get_info) = gadget_table.get::<LuaFunction>("GetInfo") {
            let info: LuaTable = get_info.call(&gadget_table)?;
            let name: String = info.get("name").unwrap_or_else(|_| String::new());
            if !name.is_empty() {
                let msg = format!("{name}: send me the data!");
                let _ = recv.call::<()>((&gadget_table, msg, 0));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_types::SmfHeader;

    #[test]
    fn simple_lua_heightmap() {
        let header = SmfHeader::new_flat(128, 128, 0.0, 100.0);
        let heightmap_len = header.heightmap_len();
        let mut map = ParsedMap {
            header,
            heights: vec![50.0; heightmap_len],
            features: vec![],
            metalmap: vec![0; 64 * 64],
        };

        let gadget_source = r#"
            function gadget:GetInfo()
                return { name = "test", desc = "test", author = "test", date = "", license = "", layer = 0, enabled = true }
            end
            function gadget:Initialize()
                Spring.SetHeightMapFunc(function()
                    for z = 0, 128, 8 do
                        for x = 0, 128, 8 do
                            Spring.SetHeightMap(x, z, 200)
                        end
                    end
                end)
            end
        "#;

        let lua_files = vec![LuaFile {
            path: "LuaRules/Gadgets/test.lua".to_string(),
            content: gadget_source.to_string(),
        }];

        let results = apply_lua_heightmap_gadgets(&mut map, &lua_files);
        assert_eq!(results.len(), 1);

        // The gadget set all accessible heights to 200.
        assert!((map.heights[0] - 200.0).abs() < 0.01);
    }

    #[test]
    fn palladium_gadget() {
        let sd7_path = [
            "kernel-panic/assets/maps/Palladium_0.5_(beta).sd7",
            "assets/maps/Palladium_0.5_(beta).sd7",
        ]
        .iter()
        .map(std::path::Path::new)
        .find(|p| p.exists());
        let Some(sd7_path) = sd7_path else {
            eprintln!("Skipping: Palladium not found");
            return;
        };

        let extracted = crate::sd7_archive::load_map_archive(sd7_path).unwrap();
        let mut parsed = crate::smf_parser::parse_smf(&extracted.smf_data).unwrap();

        // Before gadget: all heights should be the same (flat map).
        let initial_height = parsed.heights[0];
        assert!(
            parsed
                .heights
                .iter()
                .all(|&h| (h - initial_height).abs() < 0.01),
            "Palladium should be flat before gadget execution"
        );

        let results = apply_lua_heightmap_gadgets(&mut parsed, &extracted.lua_files);
        assert_eq!(
            results.len(),
            1,
            "Should execute exactly one heightmap gadget"
        );

        // After gadget: heights should vary (platforms at different levels).
        let min_height = parsed.heights.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_height = parsed
            .heights
            .iter()
            .cloned()
            .fold(f32::NEG_INFINITY, f32::max);
        let height_range = max_height - min_height;

        eprintln!(
            "Palladium after gadget: height range {min_height:.0}..{max_height:.0} (delta={height_range:.0})"
        );
        assert!(
            height_range > 10.0,
            "Height range should be significant after gadget: got {height_range}"
        );
    }
}
