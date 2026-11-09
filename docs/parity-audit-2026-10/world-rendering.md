# World and unit rendering parity audit

Scope: unit models and materials, the Bad Block and other features, build and cloak looks, terrain, water, sky and fog, decals, icons, selection and health overlays, and the minimap. Everything is compared against Recoil (`upstream/RecoilEngine`) plus KP 4.912 (`upstream/Kernel-Panic`), and both sides were read for every finding.

Two forks of this audit covered the environment and the overlays/UI; their findings are merged here. I spot-checked their key citations: terrain `unlit`, the fog default 0.999, the decal size formula, the health-bar constants, the icon-distance formula and EngineOptions GhostedBuildings.

Which KP widgets are active matters for several findings. `LuaUI/Widgets/kp_settings.lua:14-52` enables only a whitelist, which includes:
- HealthBars
- HighlightSelectedUnits
- KP Geos Highlight
- MiniMap Start Boxes
- CustomFormations2

Everything else is disabled, including Redo Team Colors, BuildETA, RelativeMinimap and CameraShake. The same widget also sets `UnitIconDist=500`, `GroundDecals=1` and `CamMode=1` (`:66-71`).

## Findings (most important first)

### 1. Bad Blocks (and Central Hub's unusable sectors) are drawn at ~10 % brightness, almost black — severity: high · confidence: verified
- Original:
  - **Model and textures:** `objects3d/badblock.s3o` uses tex1 `badblock.tga` and tex2 `octa_supplement.tga`. `badblock.tga` is plain RGB art with **alpha 0 on every texel**: red sides (255,0,0) and a top with a yellow (255,255,0) frame and a red X on black. `octa_supplement.tga` is R=0 G=0 A=255, i.e. no self-illumination, no reflection, opaque.
  - **Shader rule:** the model shader uses tex1.a as the team-colour mask and tex2.r as self-illumination:
    - `texColor1.rgb = mix(texColor1.rgb, teamCol.rgb, texColor1.a);`
    - `vec3 light = sunAmbientModel.rgb + (NdotL * sunDiffuseModel.rgb);`
    - `light += texColor2.rrr; // self-illum`
    - `finalColor = finalColor * light + specular;`
    - (`cont/base/springcontent/shaders/GLSL/ModelFragProgGL4.glsl:101,118,129,131`; the legacy `ModelFragProg.glsl:104,108-109` does the same.)
  - **Result:** alpha 0 means no team colour at all. The block is its red/yellow/black art × the map's unit light. On Quad Core (`UnitAmbientColor=0.7`, `UnitSunColor=0.9`, which maphelper renames to `unitDiffuseColor`, `cont/base/maphelper/maphelper/parse_tdf_map.lua:98-100`) that is 0.7–1.6×, a fully bright red cube.
  - **As a feature:** a finished block becomes the `badblock` feature (`rts/Sim/Units/Unit.cpp:447-459`). It is drawn by the same model shader with `SetTeamColor(f->team)` (`rts/Rendering/Features/FeatureDrawer.cpp:276`), so it looks exactly like the unit.
  - **Unusable sectors:** Central Hub's `unusablesector{1-4}.s3o` (in the map archive) use the same convention. `Usector*tex1.png` is cyan/purple RGB with alpha 0 everywhere, and `UsectorXtex2.png` is R=0 A=255.
- Remake:
  - **Texture rule:** every unit and feature texture goes through `colorize_texture` (`kernel-panic/src/units/assets/meshes.rs:399-433`). It treats RGB black as "team colour" and alpha as a brightness mask: `let scale = 25 + (alpha * 230 / 255);` (`:425`).
  - **Effect of alpha 0:** a ×25/255 multiplier. Red becomes (25,0,0), yellow (25,25,0), and black becomes 10 % of the owner colour, or 10 % white for map features (`FEATURE_COLOR`, `:104`, `:120-136`).
  - **No light:** the material is `unlit: true` (`:86`, `:127`), so there is no sun term to bring it back.
  - **Paths:** built blocks use `unit_material` (`units/features.rs:226-235`), map blocks `feature_material` (`:236-245`), and unusable sectors the committed PNGs with the same bake (`:102-123`).
- In play:
  - Every Bad Block is a near-black cube with a faint tint: built ones, and the map-placed ones on Quad Core, Dual Core, Corrupted Core and DigitalDivide.
  - The original shows a bright red block with a yellow-framed red X on top.
  - Central Hub's unusable sectors are near-black instead of cyan and purple.
  - This is `docs/next.md:1` ("bad blocks render wrongly").
  - Other possible causes were ruled out:
    - **Team colour:** irrelevant (alpha 0).
    - **UVs:** match.
    - **Culling:** matches.
    - **Pieces:** the `blaser*` pieces are empty.
    - **No double draw:** the unit is despawned as the feature spawns.
  - The feature yaw (finding 25) is invisible on this symmetric texture.
- Fix: bake Spring's rule `rgb = tex1.rgb·(1−a) + team·a` (opaque) in `colorize_texture`. Then light it (finding 7); while still unlit, at least do not scale by alpha.

### 2. Terrain is drawn unlit: no map sun/ambient lighting, slope shading, specular or unit shadows — severity: high · confidence: verified
- Original:
  - **Ground shading:** `AdvMapShading` is on by default (`rts/Map/SMF/SMFGroundDrawer.cpp:46`; `SMFRenderState.cpp:108`). The ground colour is `(diffuseCol.rgb + detailCol.rgb) * shadeInt.rgb` (`SMFFragProg.glsl:381`), where `shadeInt = groundAmbientColor + groundDiffuseColor·(N·L·shadowCoeff)`, then scaled by `SMF_INTENSITY_MULT` ≈ 0.824 (`:205-206`; `GlobalRendering.h:405`).
  - **Specular and shadows:** `groundSpecularColor·pow(N·H, exp)` is added (`:405-422`; defaults 0.1/100, `MapInfo.cpp:213,221`). Shadows default to 2, meaning models cast shadows onto terrain (`ShadowHandler.cpp:32,93-97`), at `groundShadowDensity`.
  - **Flat ground versus raw texture:**
    - Quad, Dual and Corrupted Core, Central Hub, Palladium and Data Cache (ambient 0.6, sun 1, SunDir −2 7 −4): ≈1.19×.
    - Marble Madness, Memory Bank, DigitalDivide and Major Madness: 0.70×.
    - pacman: 0.59×.
    - Circular Buffer has a bluish tint and Stack Overflow a warm tint.
    - Slopes vary ±20–40 %.
- Remake: `terrain/material.rs:7-11` builds the ground as `StandardMaterial { unlit: true }`. The `DirectionalLight`/`GlobalAmbientLight` built from the map (`map_loading/mod.rs:1191-1216`) therefore light nothing, and they have `shadow_maps_enabled: false` (`:1206`).
- In play:
  - Hills, ramps and cliffs show no sun shading, and buildings cast no shadows.
  - Memory Bank and Marble Madness are about 30 % too bright, and FireStorm maps lack their slight over-brightening.
  - Circular Buffer and Stack Overflow lose their tint.
- Fix: a custom terrain material that evaluates the SMFFragProg formula on gamma-space values with the map's sun, ambient, diffuse and specular, plus a shadow-casting sun at `groundShadowDensity`.

### 3. TonyMcMapface tonemapping dims and desaturates the whole picture — severity: high · confidence: verified
- Original: Recoil's forward pipeline is LDR. The shader result is clamped into an 8-bit framebuffer with no tonemapper, so a colour drawn at 1.0 is shown at 1.0.
- Remake:
  - **Camera:** `Camera3d` + `Hdr` (`rendering/camera.rs:131-155`) with no `Tonemapping` anywhere in `kernel-panic/src`. Bevy registers `Tonemapping` as required on `Camera3d` (`bevy_core_pipeline/src/core_3d/mod.rs:108`), and its default is `#[default] TonyMcMapface` (`bevy_render/src/view/mod.rs:324-325`, Bevy fork rev 6fa5f28).
  - **Measured effect:** I sampled the shipped `tony_mc_mapface.ktx2` LUT with `tonemapping.wesl:90-91`:

    | Input (sRGB) | Output (sRGB) |
    |---|---|
    | white (255,255,255) | (196,196,196) |
    | pure red | (215,46,32) |
    | pure green | (62,193,45) |
    | pure blue | (0,53,212) |
    | the remake's Network tint as displayed, (51,128,255) | (62,121,213) |
    | 50 % grey | ≈ unchanged |

  - **Documentation:** this is not mentioned anywhere; only bloom is documented (`rendering/settings.rs:21`).
- In play: KP's whole look is pure neon on black. Every team colour, white texture detail, ground highlight and effect is ~23 % darker at the top end and visibly washed out.
- Fix: `Tonemapping::None` on the RTS camera (bloom still works; values then clamp like Spring's LDR output).

### 4. The team-colour bake inverts Spring's tex1 rule for every texture that is not "black RGB + alpha mask" — severity: medium · confidence: verified
- Original: `rgb = mix(tex1.rgb, team, tex1.a)` (shader lines in finding 1). Alpha 0 shows the texture's own RGB; alpha 1 shows pure team colour whatever the RGB is. Opaque passes also discard texels whose tex2.a < 0.5: `alpha = teamCol.a * float(texColor2.a >= 0.5)` plus AlphaDiscard (`ModelFragProgGL4.glsl:97-99`); in the legacy path, `glAlphaFunc(GL_GREATER, 0.5f)` (`ModelDrawerState.cpp:52-55`).
- Remake:
  - **Formula:** `meshes.rs:408-429` sets `brightness = max(r,g,b)`. Black becomes the team colour, other RGB is lerped toward itself by brightness, and everything is scaled by `25+α·230/255`.
  - **Opacity:** output alpha is forced to 255 ("Always opaque", `:424-429`).
  - **Doc comments:** they state the wrong convention (`:60-62`, `:397-398`).
- In play:
  - **Logic Bomb** (`minetex.tga`: white RGB at α=255 over 67 % of texels, black α=0 triangles). Spring shows team colour with black triangles; the remake shows **white** with dim team-tinted triangles.
  - **Signal** (the Terminal's airstrike plane, `Side=CPU`) and its **SIGTERM** bomb model (`signal.tga`: the orange/gold/brown body is RGB at α=0, and the `bomb` piece uses v 0.01–0.60). Spring shows a bright gold body (tex2 `solid_bright` R=255); the remake shows near-black brown.
  - **Window hourglass** (the "busy" animation):
    - `hglass0/1`: 180 vertices sample tex2 α=0 (glass, discarded in Spring) and 144 sample brown (85,68,46,α0).
    - `sand0-2`: sample grey (100,100,100,α0).
    - Spring shows a brown hourglass with grey sand. The remake shows an opaque near-black blob, with the glass faces drawn and hiding the near-black sand.
  - **Every unit's black areas** (85 % of `kernel.tga`, most of all System/Hacker/Network textures): pure black in Spring, a 10 % owner-colour glow in the remake.
- Fix: `out.rgb = mix(rgb, team, a)` in `colorize_texture`, and keep tex2.a < 128 as transparent (alpha mask / `AlphaMode::Mask(0.5)`).

### 5. Building ground decals are not drawn: no plates under bases, no Hole decal, no Debug reticle — severity: high · confidence: verified
- Original:
  - **Which units:** the FBIs set `UseBuildingGroundDecal=1`:
    - `kernel_base.tga`: Kernel and Carrier.
    - `socket_base.tga`: Socket, Terminal, Window, Obelisk, Port and Firewall.
    - `holeNEW_base.tga`: the Hole.
    - `reticle_red.tga`: the Debug (`mineblaster`, size 32).
  - **Size and rotation:** the decal spans ±`groundDecalSizeX·SQUARE_SIZE` around the unit, so 128×128 elmos for size 8 and 512×512 for the Debug (`rts/Rendering/Env/Decals/GroundDecalHandler.cpp:874-881`), rotated by build facing.
  - **Lifetime:** added at `RenderUnitCreated`, i.e. when construction starts (`:1737`). It fades over ≈1 s after death (`alphaFalloff = decaySpeed/GAME_SPEED`, `:971`).
  - **Shading:** drawn lit like the ground (`GroundDecalsFragProg.glsl:398,421,471`). `GroundDecals=1` is forced by `kp_settings.lua:66`.
  - **Debug:** `nullobject.s3o` has no geometry, so the reticle is the Debug's only visual.
- Remake: no decal code anywhere (`decal`, `_base.tga`, `reticle` not found in `kernel-panic/src`).
- In play:
  - Every KP structure sits on a black and yellow circuit plate in the original, and the Hole is mostly its ground decal.
  - A Debug being built shows its 512-elmo red blast reticle in the original; in the remake it is invisible.
- Fix: a ground-hugging textured quad per decal unit from creation (±size·8 elmos, facing-rotated, lit like the ground), faded over 1/`decaySpeed` s after death.

### 6. `unit_healthbars.lua` is not ported: bars only on selected units, wrong placement/look, no build/stun/reload/feature bars, no stun overlay — severity: high · confidence: verified
- Original (`LuaUI/Widgets/unit_healthbars.lua`, enabled by kp_settings):
  - **Which units:** every visible unit of any team (`GetVisibleUnits(-1,…)` `:1059`).
  - **Health bar:** only when HP < 100 % (`:39 drawFullHealthBars=false`, `:633`).
  - **Other bars:**
    - a grey build bar on every unit under construction (`:139,642-643`);
    - a blue paralyze bar with seconds left (`:667-683`);
    - a cyan reload bar for ≥4 s primary weapons, i.e. the Pointer (`:47,734-741`);
    - feature bars: health < 85 % and Clear progress (`:43,842,853`).
  - **Look:** billboarded at model height + 14 (`:580,773-774`), 28×3 elmos, filled from the left (`:402`).
    - Colours: red → orange → green at α 0.9 over a grey gradient (`:125-127`).
    - Text: "NN%" within ≈837 elmos (`:45,478`).
  - **Stun overlay:** stunned units also get a pulsing additive cyan overlay plus a scrolling `paralyzed.png` (`:895-930`). The DOS beam is `paralyzer=1`.
- Remake (`interaction/selection/health_bars.rs`):
  - **Which units:** selected units only (`:65,144`), shown even at full HP.
  - **Placement and size:** 30 elmos above the root (`:61`), 20×2 elmos (`:58-59`).
  - **Fill:** the fill scales about its centre (`:182`).
  - **Colours:** HP ramp (1,0,0)→(1,1,0)→(0,1,0) over 60 % black.
  - **Orientation:** `from_rotation_arc(Z, to_camera)` rolls the bar when the camera yaws (`:167`).
  - **Missing:** no other bars, no text, and nothing visual reads `Stunned`.
- In play:
  - In KP every damaged unit in a fight shows its bar, every unit under construction shows its build progress, Bad Blocks being cleared show progress, and DOS-stunned units pulse cyan.
  - In the remake you see nothing unless you select, and on tall structures (Terminal 72, Kernel 79, Obelisk 86 elmos) the 30-elmo bar sits inside the model.
- Fix: port the widget's rules (all visible units, HP < 100 % or under construction), plus the stun/reload/feature bars, placement at model height + 14, left-anchored fill, screen alignment, and the stun overlay.

### 7. Units are drawn unlit: no sun shading, no tex2 self-illumination/reflectivity, map unit lighting never read — severity: medium · confidence: verified
- Original: colour = `tex·(unitAmbient + N·L·unitDiffuse + tex2.r) + spec·tex2.g·4`, clamped (`ModelFragProgGL4.glsl:113-131`). KP's tex2 maps differ per faction:
  - **System and Network** (`solid_bright.tga`, R=255): light ≥ 1.0, so team-colour areas with partial mask are boosted ×1.7–2.6 and clamp toward full colour. An α=0.37 area shows at 0.63–0.96×team.
  - **Hacker supplements** have partial R: Worm mostly 64, Bug mostly 42, DOS 0 on 30 % of texels, Obelisk/Trojan ≈100–170. These units are visibly sun-shaded, with the sun side up to ~2× the shadow side on Quad Core light.
  - **Bad Block, Logic Bomb (`black.tga`) and unusable sectors** have R=0 and are purely sun-lit.
  - **Window supplement:** G up to 171 (reflection/specular).
- Remake:
  - **Materials:** `StandardMaterial { unlit: true }` for every unit and feature (`meshes.rs:84-88,125-128`). An α=0.37 area shows flat at 0.43×team.
  - **Map data:** `spring-map/src/map_info.rs:57-66` keeps only ground lighting, so `UnitAmbientColor`/`UnitSunColor`/`UnitShadowDensity` never reach the game.
- In play: Worm, Bug, DOS, Trojan and Obelisk look flat instead of shaded. System and Network units are darker in their mid-tones. Bad Blocks and Logic Bombs have no lit/unlit sides.
- Fix: a small custom model material (tex1, tex2, team colour, sun dir, unit ambient/diffuse) evaluating Spring's formula in gamma space. Carry the map's unit lighting in the map format.

### 8. No unit icons: no radar blips, no far-zoom icons; detected cloaked enemies drawn as full models — severity: medium · confidence: verified
- Original:
  - **Icon types:** `gamedata/ICONTYPES.TDF`, e.g. bit `round` 1, byte `losange` 2.5, kernel `cross` 3, carrier `plussign` 3.2, default `trinver`; all have `distance=0.5`.
  - **Radar blips:** a unit in radar but not in LOS is always an icon (`UnitDrawerData.cpp:304`). It shows the default icon until identified (`:263-290`), in team colour (white when selected), at the radar-error position, sized `size·0.4·√dist` (`UnitDrawer.cpp:504-520,578`).
  - **Cloaked units:** non-stealth cloaked units are radar-visible (`LosHandler.cpp:951-970`), and the Logic Bomb's `Stealth=1` is commented out.
  - **Far-zoom icons:** beyond `UnitIconDist·√750·0.5` ≈ 6850 elmos with KP's 500 (`UnitDrawerData.h:106-108`, `UnitDrawerData.cpp:451-461`).
- Remake: no icon code or assets. Enemies are drawn only within `SightDistance` (`units/mechanics/cloak.rs:429-474`), and radar-detected cloaked units are drawn as full models (`cloak.rs:266-283`).
- In play:
  - Scouting constructors (Trojan radar 768 vs sight 384; Assembler and Gateway 500/384) show enemy movement as coloured triangles in KP; the remake shows nothing past sight.
  - Mines in an enemy's radar are blips in KP, full models in the remake.
- Fix: load ICONTYPES and `icons/*.tga`, draw camera-facing icons for radar-only units and beyond the icon distance, and draw radar-detected cloaked units as blips.

### 9. Enemy-built Bad Blocks are visible through the fog of war — severity: medium · confidence: verified
- Original:
  - **Rule:** `gamedata/modrules.tdf:13` sets `featureVisibility=2` (GAIAALLIED). `CFeature::IsInLosForAllyTeam` returns `isGaia || allyteam == argAllyTeam || losHandler->InLos(pos, argAllyTeam)` (`rts/Sim/Features/Feature.cpp:128-150`). The feature drawer skips features that fail it (`FeatureDrawerData.cpp:136`).
  - **Teams:** a built block keeps its builder's team and ally team (`Unit.cpp:451`). Map features are remapped to Gaia (`Feature.cpp:112-126,246`).
  - **Result:** map and own/allied blocks are always drawn; an enemy's block is drawn only while in current LOS.
- Remake: every feature gets `Visibility::default()` (`units/features.rs:198-209`), `Feature` stores no team (`:43-53`), and the fog pass handles `With<UnitType>` only.
- In play: you see every enemy wall anywhere on the map, including behind their base, while their units there stay hidden. This settles the sensors report's open lead.
- Fix: keep the owner team on built features, and hide non-Gaia, non-allied features outside current LOS.

### 10. Scouted enemy structures vanish instead of remaining as grey "ghosts" — severity: medium · confidence: verified
- Original:
  - **Defaults:** `GhostedBuildings` is on by default (KP `EngineOptions.lua:114-119`; `GameSetup.cpp:187,634`). `leavesGhost` defaults to `IsBuildingUnit()` = immobile with a yardmap (`UnitDef.cpp:678`, `UnitDef.h:67-68`). In KP that is Kernel, Hole, Carrier, Socket, Terminal, Window, Obelisk, Port, Firewall, Exploit, Logic Bomb, Debug and Bad Block frames.
  - **Live ghosts:** a building that leaves LOS (`UnitDrawerData.cpp:683-692`) is drawn with its static model at colour ×0.6, α 0.4·0.4 = 0.16, or ×0.9 with α 0.25 while in radar (`UnitDrawer.cpp:1811-1818`).
  - **Dead ghosts:** a building destroyed while unseen stays as a ghost until the spot is seen again (`UnitDrawerData.cpp:593-644`, `UnitDrawer.cpp:1750-1751`).
- Remake: fog without memory (`cloak.rs` `update_fog_visibility`; FEATURES.md §24 documents "no memory" but not as a deliberate departure); no ghost code.
- In play: an enemy base you scouted disappears the moment your scout leaves, instead of staying as dim translucent models (and minimap icons).
- Fix: per viewer, remember enemy building units last seen and draw them with a 0.6-grey, α≈0.16 material until re-seen.

### 11. Selection highlight: health-coloured additive glow plus outline vs owner-tinted brightening — severity: medium · confidence: verified
- Original (`LuaUI/Widgets/gui_hilight_selectedunits.lua`):
  - **Glow pass:** each selected unit is drawn again raw and untextured (`glUnit(u,true)`, `:218`), additive `SRC_ALPHA, ONE` (`:172`), with polygon offset −2.
  - **Colour:** from health — green at full HP, yellow at 50 %, red near death (`:208-215`); α 0.25, or 0.125 when cloaked (`:222-223`).
  - **Outline:** 3-px point and line passes at polygon offset (10,100), which read as a thick outline (`:97,185-191`).
  - **No boxes, no hover:** the engine's unit boxes are hidden (`unitBox 0 1 0 0`, `:109-125`), and there is no hover highlight.
- Remake: `interaction/selection/highlight.rs:29-31,257-283` brightens the material ×2.5 when selected (×1.5 on hover, a documented extra), blended 40 % toward the owner colour. No outline, no health link.
- In play: a selected KP squad glows with a green outline that turns yellow, then red, as units are hurt. In the remake it is just brighter in its own colour.
- Fix: an additive flat health-colour pass (α 0.25/0.125) plus an outline pass.

### 12. Network buildees miss their scrolling build veil (`network_build.lua`) — severity: medium · confidence: verified
- Original (`LuaRules/Gadgets/network_build.lua:13-16,28-33,46-82`):
  - **Which units:** every unit under construction whose builder is a Carrier (Packet, Connection, Flow, Gateway) or a Gateway (Port, Firewall, Bad Block, Logic Bomb, Debug) (`gamedata/SIDEDATA.TDF:116-129`).
  - **Draw:** an extra `gl.Unit(u,true)` each frame, while in LOS:
    - texture `network_buildeffect.tga`: left half grey 128 at α 128 with a white edge, right half transparent;
    - object-linear texgen `S = 0.25·z/r + 0.25 − 0.5·progress`;
    - polygon offset −2;
    - colour `1−(1−team)·o²`, with o random in 0–0.5 per frame.
  - **Result:** a flickering whitish translucent shell over the not-yet-built part, receding from back to front as the build progresses.
  - **The unit itself** is drawn normally (see the alpha-threshold note under "Checked and matching").
- Remake: nothing like it (`network_buildeffect` or `network_build.lua` appear nowhere in `kernel-panic/src`). Listed as a known gap in `docs/script-and-weapon-parity.md:165-167`.
- In play: Carrier and Gateway products appear solid from the first frame, with no materialising veil.
- Fix: per buildee, a second draw of its piece meshes with a translucent material whose texture coordinate is `0.25·z/r + 0.25 − 0.5·progress` in model space, with a flickering near-white tint.

### 13. Model projectiles (Pointer "Geometric", Terminal SIGTERM bomb) are flat light grey instead of textured — severity: medium · confidence: verified
- Original: model projectiles are drawn by the model shader with the model's textures and the owner's team colour (`rts/Rendering/Env/Particles/ProjectileDrawer.cpp:711-735,945-958`: `CUnitDrawer::SetTeamColor(wp->GetTeamID()); … wp->model->DrawStatic();`).
  - `octashot.s3o` is `octashot.tga`, a uniform (192,0,192) at α 0, with `octa_supplement` (R=0): a magenta, sun-lit octahedron.
  - `sigterm.s3o` is `signal.tga` + `solid_bright`: a gold body with team-coloured fins.
- Remake: `spawn_projectile` uses the mesh but a flat `get_or_create_tiled(tdf_color(weapon.rgb_color), …)` (`units/weapon_fx/spawn.rs:1022-1050`). Neither `Geometric` nor `SigTerm` sets `rgbcolor` (`weapons/retroweapons.tdf:104-140,569-600`), so the material is `(0.7,0.7,0.7)` unlit (`weapon_fx/shared.rs:572-578,632-640`).
- In play: Pointer shots are light-grey octahedra instead of magenta ones, and airstrike bombs are grey instead of gold with team fins.
- Fix: draw model projectiles with the same tex1/tex2 material as units (owner colour).

### 14. Map detail textures (fine circuit traces) are ignored — severity: medium · confidence: verified
- Original:
  - **Formula:** `detailCol = texture2D(detailTex, worldPos.xz*0.02)*2−1` is added to the ground (`SMFFragProg.glsl:157-158,381`), so the texture repeats every 50 elmos.
  - **Which maps:** nine set their own `DetailTex`:
    - Quad, Dual and Corrupted Core and Central Hub use `*Circuitry.png`;
    - Data Cache L1, Palladium, DMA 0.5c/e and Spooler use `MemoryCircuitry.png`.
  - KP's `RESOURCES.TDF` neutral `grey.tga` is only the fallback (`MapInfo.cpp:377-380`).
  - **QuadCircuitry:** 5 % of texels add +1.0 green (bright traces) and 7 % add +0.21 green.
- Remake: `DetailTex` is read neither by the bake nor by the game (`spring-map/src/map_info.rs` has no field).
- In play: zoomed in, those maps lack the green and blue circuit traces over the ground.
- Fix: ship the nine PNGs keyed by map, and add `(tex·2−1)` at world·0.02 with a repeating sampler.

### 15. Water is not drawn on the two water maps, where it deals damage — severity: medium · confidence: verified
- Original:
  - **Renderer:** reflective by default (`IWater.cpp:26-28`).
  - **Submerged ground:** shaded toward `waterBase − absorb·depth`, clamped at `minColor` (`SMFFragProg.glsl:220-248`; `SMFRenderState.cpp:115`).
  - **Water maps:** Speed Balls 16 Way (62 % of the heightmap below 0) and DigitalDivide PT2 (28 %).
- Remake: `map_events/water.rs` applies only `WaterDamage`. No water surface, no absorption tint.
- In play: DigitalDivide's damaging stream looks like dry painted ground, and Speed Balls' lanes lack their reflective surface. The hazard is hard to read.
- Fix: a y=0 water plane with the map's water colours (per-map table), plus absorption darkening of submerged ground.

### 16. Fog: wrong defaults (DigitalDivide's haze missing), linear-space mixing weakens KP's black distance fog — severity: medium · confidence: verified (defaults) / likely (magnitudes)
- Original:
  - **Defaults:** `fogStart 0.1`, `fogEnd 1.0`, `fogColor (0.7,0.7,0.8)` (`MapInfo.cpp:137-139`).
  - **Range:** fog spans `GetFarPlaneDist()·start…end` (`ISky.cpp:60-61`; far plane from the camera, `Camera.cpp:336-353`).
  - **Blend:** `mix(fogColor, color, fogFactor)` on gamma values (`SMFFragProg.glsl:437`; models likewise).
  - **Maps:** DigitalDivide sets no fog keys, so it gets grey-blue haze from 10 % of the far plane. FireStorm maps use black fog with `FogStart=0.001`.
- Remake:
  - **Defaults:** `fog_start 0.999`, `fog_color [0,0,0]` (`spring-map/src/map_info.rs:47-49`); the DigitalDivide kpmap carries exactly that.
  - **Range:** a fixed end of `max(diag·1.1, 4000)` (`map_loading/mod.rs:1239-1244`).
  - **Blend:** linear-space fog (`bevy_pbr/src/render/fog.wesl:36-37`).
- In play: DigitalDivide looks crisp instead of hazy. On Quad Core, mid-screen ground is ≈84 % bright in the remake versus ≈70 % in KP.
- Fix: engine defaults for omitted keys, gamma-space fog, and an end distance from the far-plane formula.

### 17. Geovent smoke look and rate differ — severity: medium · confidence: likely
- Original:
  - **Rate:** one `CGeoThermSmokeProjectile` per vent per sim frame (30/s; `Feature.cpp:663-664,730`), hidden only while a needGeo unit stands on the vent (`:714-717`).
  - **Drift:** with the wind (`GeoThermSmokeProjectile.cpp:40-46`).
  - **Texture:** random among `NumSmokeTextures()`. KP ships only smoke00–02 (small green 0/1 glyph pairs), and the engine's grey smoke03–11 fill the rest.
- Remake: `EMIT_INTERVAL = 1/18 s` ("KP slows that down", `terrain/geovent.rs:104-107`), procedural 8×8 green glyphs filling the quad, additive (`:196-202`), no wind, and emission stops when a build is ordered (`VentClaim`).
- In play: the original shows a dense, mostly grey column leaning with the wind with occasional green digits; the remake shows a sparser straight column of large green digits.
- Fix: a uniform pick from 12 textures, emission every sim frame, wind drift, stopping only while a needGeo building exists.

### 18. Minimap look — severity: low · confidence: verified
- Original:
  - **Background:** the minimap image × sun shading (`MiniMapFragProg.glsl`; `SMFShadingTextureFragProg.glsl`), with water tinted by depth.
  - **Units:** ICONTYPES icons at 2.5 px × the icon size (`MiniMap.cpp:63-69,539-544`). Bit/Bug/Packet are 5-px rounds; Kernel and Hole are 15 px.
  - **Colours:** team colour, white when selected (`UnitDrawer.cpp:418`), plus radar blips and ghosts.
  - **Frustum:** a white 1.5-px line over a black 2.5-px line (`MiniMap.cpp:1500-1512`).
- Remake (`ui/minimap.rs`): point-sampled ground texture with no shading (`:115-117,352-389`). Uniform 3×3-px owner-coloured dots for spotted units (`:182,201-217`), with no selected colour. A 1-px white frustum on the y=0 plane (`:233-249`).
- In play: KP's minimap shows bases as big distinct shapes among tiny spam dots, and your selection in white.
- Fix: icon sprites at 2.5·size px, white when selected; shaded background.

### 19. Camera can't zoom out to the whole map and isn't KP's overhead tilt (outside this slice; also coordinator C9) — severity: low · confidence: verified
- Original: `CamMode=1` (`kp_settings.lua:67`), the overhead controller.
  - **Max distance:** 9.5 × map width in squares (`OverheadController.cpp:63`), ≈4864 elmos on Quad Core and Central Hub.
  - **Tilt:** a fixed tilt 0.464 rad from vertical (`OverheadController.h:52`), with no yaw.
- Remake: orbit distance 150–3000 (`rendering/camera.rs:104-107`), pitch 45°, free yaw.
- In play: a 4096-elmo map can never be seen whole.

### 20. No weapon or build range circles — severity: low · confidence: verified
- Original: selected armed units show a ground range circle while Attack is armed or is the hover default (`GuiHandler.cpp:3894-3925`; `cmdcolors.txt` `rangeAttack 1.0 0.3 0.3 0.4`). Builders show build-distance circles (`:3789-3810`), and Shift-hover shows a unit's ranges (`:3727-3760`).
- Remake: none.

### 21. Friendly cloaked units are drawn at 50 % opacity; the engine uses 18–30 % — severity: low · confidence: verified
- Original: cloaked units go to the alpha pass (`UnitDrawerData.h:125`: `IsAlpha = IsCloaked()`). `UnitTransparency` defaults to 0.7 (`UnitDrawer.cpp:62`), so `alphaValues.x = 0.3` (`ModelDrawerState.cpp:459`).
  - **GL4 drawer (default):** `SetColorMultiplier(0.6f)` + `SetTeamColor(0, alphaValues.x)` (`UnitDrawer.cpp:1720-1721`) gives α = 0.3·0.6 = **0.18**.
  - **Legacy drawer:** 0.3·tex2.a.
- Remake: `CLOAK_FADE_ALPHA: f32 = 0.5` (`units/mechanics/cloak.rs:287`).
- In play: your own Logic Bombs and submerged Worms look much more solid than KP's faint ghosts.
- Fix: α 0.18 (or 0.3), lit like opaque units.

### 22. "Ground lift" moves several units vertically away from where Spring draws them — severity: low · confidence: verified
- Original: a unit's model origin is drawn at its position, on the ground for ground units. No script lifts the root at Create. Static lowest vertices:

  | Model | Unit | Lowest vertex |
  |---|---|---|
  | `assembler.s3o` | Assembler (a disc at y 16) | +9.63 |
  | `trojanNEW.s3o` | Trojan (centre at 32) | +9.53 |
  | `network_builder` | Gateway | +3.07 |
  | `network_spam` | Packet | +2.98 |
  | `network_big` | Connection | −9.62 |
  | `network_medium` | Flow | −4.72 |
  | `ball` | Bit | −1.16 |

- Remake: `spawn_unit_facing` lifts the root by `-mins.y` (`units/lifecycle/spawning/mod.rs:446-469`, `s3o_mount.rs:102-104`), except for the Carrier and Byte. `GroundLift` keeps that offset (`interaction/movement.rs:788-814`).
- In play: Assemblers and Trojans sit on the ground instead of hovering ~10 elmos up. Connections ride ~10 elmos higher, Flows ~5 higher, and Gateways and Packets ~3 lower. The collision/selection sphere moves with the root.
- Fix: lift 0 for all units, as already done for Carrier and Byte.

### 23. Unit textures have no mipmaps — severity: low · confidence: verified
- Original: S3O textures get a full mip chain with trilinear filtering (`S3OTextureHandler.cpp:175` → `CBitmap::CreateMipMapTexture()`, `reqNumLevels=0`, `linearMipMapFilter=true`; `Bitmap.cpp:1856-1865`, `Bitmap.h:20-29`).
- Remake: `create_rgba8_image` uses `Image::new` (one level) with a mipmap filter that has nothing to filter (`meshes.rs:440-463`).
- In play: when zoomed out, the thin team-colour line art on 256²–512² textures (Window text, Kernel and Socket patterns) shimmers as units move.
- Fix: generate mips when baking the coloured texture.

### 24. Sky: skyboxes and the procedural sky aren't drawn; the clear colour treats the map's gamma value as linear — severity: low · confidence: verified
- Original:
  - **Which sky:** `CSkyBox` when the map sets `skyBox`, else `CModernSky` with clouds and a sun spot (`ISky.cpp:75-79`). Quad, Dual and Corrupted Core and Central Hub ship skyboxes; Speed Balls uses `CorePrime.dds`.
  - **Others:** DigitalDivide gets red clouds on dark red, and pacman a blue sky with clouds.
- Remake: `ClearColor(Color::linear_rgb(sky…))` only (`map_loading/mod.rs:1193`; `plan.md:629` "Fix the skybox"). FireStorm's 0.01 "black" shows as ≈25/255.
- Fix: draw cube maps and a ModernSky-like gradient; use `Color::srgb` for the clear colour.

### 25. Map features and built Bad Blocks are yawed 180°; built ones ignore their build facing — severity: low · confidence: verified
- Original: a feature's frame comes from its heading like a unit's, with heading 0 facing +Z. Built blocks take the unit's `heading, buildFacing` (`Unit.cpp:451`); map features take their SMF `rotation` (`FeatureHandler.cpp:95`).
- Remake: `Transform::…with_rotation(Quat::from_rotation_y(PI))` for every feature (`units/features.rs:204-207`). The π only cancels for units, where `attitude()` already turns −Z onto the heading (`interaction/ground_move.rs:2229-2233` plus the `ModelRoot` π in `spawning/mod.rs:612-617`).
- In play: invisible on the Bad Block, whose texture is 180°-symmetric. Unusable sectors' side stripes appear mirrored.
- Fix: identity rotation, or the feature's heading.

### 26. Map-edge border skirts are missing — severity: low · confidence: likely
- Original: `DrawBorder` draws darkened walls from the map edge down to `min(minHeight, −500)` (`SMFGroundDrawer.cpp:341-367`; `SMFBorderFragProg.glsl` ×110/255).
- Remake: the terrain just ends, and the camera can go 25 % past the edge.
- In play: high plateau maps (Data Cache L1, DMA) float as a slab over grey void.

### 27. Command and selection markers look different — severity: low · confidence: verified
- **Drag box:** KP draws it light cyan `mouseBox 0.45 0.95 1.0 0.8`, width 1.49 (`cmdcolors.txt:18-19`); the remake draws a white 1-px box (`interaction/selection/core.rs:282,297`).
- **Queued targets:** KP shows the command's cursor sprite at α 0.5, with stippled 2-px lines and no click marker (`cmdcolors.txt:36-38,55-80`; `CursorIcons.cpp:84-121`). The remake shows 6-elmo rings plus 1.5 s click rings (`interaction/movement.rs:1094-1119`; `right_click.rs:95-160`).
- **Formation drag:** CustomFormations2 draws a stippled fading line along the drag, also on the minimap (`unit_customformations2.lua:217-220,663-720`); the remake shows yellow rings at the slots (`right_click.rs:648-716`).

### 28. No loading screen — severity: low · confidence: verified
- Original: `LuaIntro/Addons/CircuitGrowth.lua` shows an animated circuit-growth pattern, a wobbling cyan "KERNEL PANIC" vector logo with a green tail, and the load message and versions.
- Remake: only the Menu and InGame states (`game_setup.rs:21-25`).

Questionable documented deviations (one line each):
- **Per-faction owner colours:** System green, Hacker red, Network blue, then alternates (`team_colors.rs`, FEATURES.md). KP uses per-player lobby colours or the engine palette by team index (`TeamBase.cpp:30-42`), and `gui_redo_teamcolors.lua` is disabled. This is a deliberate design.
- **Bloom:** at 0.15 (`rendering/settings.rs:21`), documented.
- **Hover highlight:** the ×1.5 hover highlight (FEATURES.md:104-105) has no KP counterpart.

## Doc drift
- **Texture convention comments:** `units/assets/meshes.rs:60-62` and `:397-398` describe Spring's tex1 convention as "RGB=black → team colour, alpha → brightness". The engine uses tex1.a as the team-colour mask (`mix(rgb, team, a)`).
- **texture2 description:** `units/content/model.rs:29-30` calls texture2 "typically team-color or specular map". It is self-illumination (R), reflectivity (G) and alpha mask (A).
- **Alpha fade:** `docs/script-and-weapon-parity.md:165-166` says "Hacker and Network buildees still alpha-fade in", but the code draws them opaque (`Faction::emerge_style` is always `Rise`, `units/components.rs:85-97`). `units/lifecycle/spawning/emerge.rs:39-42` still says `EmergeStyle::Fade` "mirrors upstream's lua_SetAlphaThreshold", which is inert on Recoil.
- **Feature yaw comment:** `units/features.rs:204-205` justifies the π yaw by Bevy's −Z forward. With no `look_to` on features, it rotates the model instead (finding 25).
- **Faction colours:** `FEATURES.md:712-713` says faction colours are used for health bars; health bars use the HP ramp. `FEATURES.md:242-246` says minimap dots are faction-coloured; they use the owner colour (`minimap.rs:182,203`).
- **Health bar in the factory:** `FEATURES.md:439-441` says the factory health bar "goes up to 100%". Units spawn at full HP (`spawning/mod.rs:485`, `Health::full`).
- **Sky:** `FEATURES.md:20` says "The sky is uniform black (no skybox)". The clear colour is the map's `sky_color` (dark red on DigitalDivide, light blue on pacman).
- **Fog keys:** `plan.md:1122` says `fog_start`/`fog_color` are "parsed … but never applied". `apply_fog` applies both; only `cloud_density` is unused.
- **Geovent smoke:** `terrain/geovent.rs:104` says "KP slows that down", but no KP code throttles geovent smoke. The module header (`:1-4`) says "grey smoke puffs", while the code draws green glyphs.
- **Map lighting:** `map_loading/mod.rs:1195-1216` sets up the map's sun and ambient lights, but every material is unlit, so they have no effect.
- **Health bar orientation:** `health_bars.rs:165` says the rotation keeps Y as up; `from_rotation_arc` rolls the bar.

## Checked and matching
- **Nanoframes:** `ShowNanoFrame=0` on every unit, so the engine draws buildees as normal models (`UnitDrawer.cpp:296` `noNanoDraw`). The remake draws them complete, with script-driven sinks (`Rise`).
- **Alpha-threshold dissolves:** `lua_SetAlphaThreshold` dissolves are inert on Recoil. `set_alpha_threshold.lua`'s `DrawUnit` returns nil, and nil means "don't suppress", so the engine also draws the unit normally (`LuaHandleSynced.cpp:213-241`, `UnitDrawer.cpp:288`). The GL4 batch ignores `luaDraw` entirely. The remake ignores them too.
- **UV orientation:** the engine loads TGAs top-left (`Bitmap.cpp:1307-1308`). The remake's bake flips bottom-left TGAs to top-left (`git show a8567de:spring-unit-mesh/src/tga.rs`).
- **Winding and yaw:** CCW back-face culling for S3O (`ModelDrawerState.cpp:49-50`) matches Bevy's default. Unit yaw at heading 0 maps model axes to world axes in both, with no mirroring.
- **Piece hierarchy:** parent offsets accumulate (`S3OParser.cpp:163`); the remake uses piece entities with local offsets.
- **Bad Block build-up:** base sunk `[-8]` = 20 elmos, lifted with `BUILD_PERCENT_LEFT`, lasers from below 99 % (`badblock.bos`), matching `animation/units/badblock.rs`.
- **Bad Block becoming a feature:** the feature appears at the unit position and the unit is removed without a death blast (`Unit.cpp:447-459`), matching `features.rs:309-339`. Nothing is drawn twice.
- **Map features:** they are Gaia and always drawn (`Feature.cpp:112-126,246`); the remake always draws them.
- **Fog on models:** fog reaches unit models in both (model shader `fogFactor`; `StandardMaterial` `fog_enabled` also applies to unlit materials).
- **Explosion scars:** KP's `scar1-4` are fully transparent (`kpsfx/transparentblack.tga`), so none are visible in either.
- **Unit tracks:** no default unit sets `leaveTracks`, so neither draws tracks.
- **Geovents:** they have no model (`DRAWTYPE_NONE`). The placement highlight (`kp_geoshighlight.lua` vs `ui/hud/geo_highlight.rs`) matches: 64-elmo squares, 0.4 s blink, additive (0.3, 1, 0.4). The puff constants match too: size 6, growth 0.35, rise 2.
- **Hex Farm:** no sky and no water, shown black in both.
- **Unusable sectors:** Central Hub's sectors use the Bad Block geometry, as `features.rs` assumes.
- **Engine selection boxes:** hidden by KP (`unitBox` alpha 0); the remake draws none.
- **Command queues:** always drawn for selected units in both (`cmdcolors.txt:34`).
- **Health bars:** the engine draws none of its own; KP's come only from the widget.
- **Disabled widgets:** BuildETA, RelativeMinimap, Redo Team Colors, CameraShake and SmoothScroll are off in KP and absent in the remake.

## Unverified leads
- **Shared team colour:** `colorize_texture` writes `LinearRgba` components as bytes into an `Rgba8UnormSrgb` texture (`meshes.rs:400-403,446`). A unit's tint therefore displays at the sRGB value equal to its declared linear numbers, while consumers that use the true linear colour show the same "faction colour" lighter. Settle by comparing a unit with its explosion particles on screen.
- **Drawer path for cloaked alpha:** whether the GL4 or the legacy drawer is active decides 0.18 vs 0.3 for cloaked units. Either way it is well below 0.5.
- **Build HP:** the engine starts buildees at 0.1 HP and adds HP with build power (`Unit.cpp:270`, `:2016/2044`), while the remake spawns them at full HP. This matters for the build/health bars and belongs to the production/combat audit.
- **Engine settings:** whether KP's installer `springsettings.cfg` changes `Shadows`, `Water` or `AdvMapShading` from the engine defaults; the file is not in the tree.
- **Water look:** the exact reflective water on Speed Balls (reflecting the CorePrime skybox) and DigitalDivide's plane colour were not reconstructed.
- **Detail texture visibility:** how strongly the detail traces show at typical zoom depends on mip filtering.
- **Not compared:** `kp_buildbar.lua:472-505` (the hovered factory's queue drawn in the world) against the remake's `draw_selected_factory_orders`, and the minimap frame, buttons, pings and map marks.
