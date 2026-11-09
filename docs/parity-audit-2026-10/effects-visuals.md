# Weapon and effect visuals parity audit

Scope: projectile/beam drawing per KP weapon, CEG runtime, build beams, death debris/bursts, ground flashes, geovent smoke, Lua-drawn effects. Engine = `upstream/RecoilEngine/rts` (authority), mod = `upstream/Kernel-Panic`, remake = `kernel-panic/src`. All findings below were read on both sides. Texture facts come from decoding the TGA files (`bitmaps/kpsfx`, `bitmaps/smoke`, engine `cont/base/bitmaps/bitmaps`).

## Findings (most important first)

### 1. Directional CEG particles are rotated 90°: `hline` tracers come out as bars across their motion — severity: high · confidence: verified
- Original: `CSimpleParticleSystem::Draw` (`Rendering/Env/Particles/Classes/SimpleParticleSystem.cpp:144-149`) builds `ydir = zdir.cross(p->speed)`, `xdir = ydir.cross(zdir)`, so `xdir` is the screen-projected velocity, and `DoParticleDraw` (`:116-132`) maps texture U (`xstart→xend`) along `xdir`. `horizontalline.tga` is a 2-px line along U, so an `hline` tracer is a streak **along** its flight. `verticalline.tga` (`vline`) is a bar across it.
- Remake: `ceg.rs:1046-1053` sets `xdir = zdir.cross(fwd)` (perpendicular) and `ydir = xdir.cross(zdir)` (along velocity; the comment says "align Y with velocity"). `push_billboard` (`ceg.rs:206-226`, `BILLBOARD_UVS` `:204`) maps U along `xdir`. U therefore runs across the motion: the axes are swapped.
- In play: these layers have `directional=1` with `hline`: `oldskool` tracers (every Byte MegaBeam hit), `network_shot` (every Packet hit), `corruption_stun` (DOS hits), `oldskool_impact` (Pointer hits), and `oldskool_big`/`oldskool_vbig` (medium and large deaths). Upstream these are radial speed lines shooting out of the blast. The remake draws a ring of short tangential dashes. `oldskool_death`'s `vline` tracers (every small-unit death) flip the other way, into radial streaks.
- Fix: swap the two axes in the directional branch (`xdir` = projected velocity, `ydir = zdir × v̂`). Equivalently, pass `(ydir, xdir)` to `push_billboard`.

### 2. DOS_Beam drawn as a plain BeamLaser: a 240-elmo green lens flare, dense static tiles — severity: high · confidence: verified
- Original: `[DOS_Beam]` sets `largebeamlaser=1; laserflaresize=0; scrollspeed=4; texture1=dosray; texture2=black` (`weapons/corruptionweapons.tdf:84-93`). `CLargeBeamLaserProjectile::Draw` sizes both flares from `thickness * flaresize` (`LargeBeamLaserProjectile.cpp:121,246`), so with `flaresize=0` there is no flare. Tiles are `tilelength` long (default 200, `WeaponDef.cpp:264`) and scroll: `startTex = 1 - frac(modGameTime*scrollspeed)` (`:115,124,183-202`).
- Remake: `large_beam_laser` and `scroll_speed` are parsed but never read. `laser_flare_size()` (`weapon_fx/spawn.rs:625-631`) turns the authored `0` into the default 15, so `flare_material` (`:719-735`) adds a camera-facing `flare.tga` of half-size 8·15 = 120 elmos, tinted (0, .5, 0), drawn at the emitter (`tick.rs:267-279`). The body tiles every 56 elmos (`spawn.rs:683-693`) and never scrolls.
- In play: every DOS shot shows a 240×240-elmo green lens flare (`flare.tga` has a white core) on top of the DOS, which upstream never draws. The upstream binary-digit stream also flows toward the target in 200-elmo tiles. The remake shows a static stream 3.6× denser.
- Fix: read absent and explicit-zero `laserflaresize` separately (spring-tdf `f32_or("laserflaresize", 15.0)`). Route `largebeamlaser` beams through a large-beam path: no flare when `flaresize=0`, `tilelength` tiles, and `scrollspeed` scroll.

### 3. Obelisk's WMD has no shell, no trail and no impact effect — severity: high · confidence: verified
- Original: `[Infection]` is a ballistic Cannon (`velocity 800, myGravity .4`) with `explosiongenerator=custom:corruption_infection; cegTag=corruption_WMDtrail` (`corruptionweapons.tdf:386-418`). The engine fires it at the command target. The shell trails red squares each frame (`ExplosiveProjectile.cpp:53-67`). On impact `corruption_infection` plays a red shockwave growing to ~420 elmos and 40 `CExpGenSpawner` timers (every 10 frames, about 13 s) of fog-square clouds (`gamedata/explosions/corruption_infection.tdf`). `areadenial.lua:30-52` creates the zone at the explosion.
- Remake: `process_command_fire` spawns the `AreaDenialZone` at the click instantly (`mechanics/command_fire.rs:840-846`). `ability_for(Obelisk)` has `explosion_ceg: None`, with the comment "Infection's gas has no CEG upstream" (`:1623-1641`). `CegKind::CorruptionInfection`/`CorruptionWMDtrail` are never used outside the defs.
- In play: an Obelisk cast shows nothing at all, neither the lobbed shell, its trail, the shockwave nor the ~13 s fog. The zone also starts immediately instead of after the 2–3 s flight. Gameplay timing overlaps the gameplay audit.
- Fix: fire the Infection as a real Cannon projectile, as `tick_nx_casts` does for the NX, and spawn the zone from its impact. At minimum, set `explosion_ceg: Some(CorruptionInfection)`.

### 4. Every `explode` piece spawns an invented team-coloured glowing sphere — severity: high · confidence: verified
- Original: `CUnitScript::Explode` adds only `CHeatCloudProjectile(nullptr, absPos, 0, 30, 30)` (`Sim/Units/Scripts/UnitScript.cpp:906-907`), drawn with `heatcloudtex` (`HeatCloudProjectile.cpp:103-140`). KP maps `heatcloud=kpsfx/black.tga` (`gamedata/RESOURCES.TDF:62`), which is opaque black with vertex alpha 1/255. Under `GL_ONE, GL_ONE_MINUS_SRC_ALPHA` that draws nothing. KP's `Killed()` scripts only use SHATTER/FALL, so the pieces themselves are the only visible debris.
- Remake: `animation/mod.rs:974-992` calls `spawn_death_particle` for every `FxEvent::Explode`. That is a team-coloured additive ico-sphere (`:1189-1209`, `AlphaMode::Add`) scaled from 2 to 22 elmos over 0.4 s (`:1246-1266`). FEATURES.md:416-419 describes it as a feature.
- In play: every death puts several bright green, red or blue balls (one per exploded piece) over the pieces and the CEG. KP shows only the flying or shattering pieces and the `ExplodeAs` CEG.
- Fix: drop `spawn_death_particle`. If anything, emulate the heat cloud, which is invisible with KP's textures.

### 5. Weapon effects are drawn through the fog of war — severity: high · confidence: verified
- Original: `CProjectileDrawer::UpdateDrawFlags` skips any projectile or particle failing `CanDrawProjectile` (`ProjectileDrawer.cpp:384,594-600`): spectator, allied owner, or `losHandler->InLos(p)` (air LOS for CEG particles, both beam ends for beams; `Sim/Misc/LosHandler.h:220-230`). The Lua arcs check LOS too (`network_arceffect.lua:142-149`).
- Remake: beams, bolts, CEG particles, flames, spikes, trails and arcs go into global per-material batch meshes (`weapon_fx/batch.rs`), and projectile entities carry no visibility logic. The fog in `mechanics/cloak.rs` hides only `With<UnitType>` entities.
- In play: with fog on, a player sees enemy build beams, Gateway arcs, Flow missiles, NX/SIGTERM fire and AI-vs-AI fights anywhere on the map. Upstream hides them, apart from `alwaysVisible` CEGs (SIGTERM, WMD smoke). This leaks information in multiplayer.
- Fix: give each effect its owner's ally team and skip quads (or entities) whose position isn't in the local team's LOS, when fog applies and the owner isn't allied.

### 6. Impact CEGs of traveling shots fire straight up instead of along the shot — severity: medium · confidence: verified
- Original: `CWeaponProjectile::Explode` passes `.dir = impactDir.SafeNormalize()` with `impactDir = speed` (`WeaponProjectiles/WeaponProjectile.cpp:200,232`). `oldskool_impact` (Pointer's `Geometric`, `retroweapons.tdf:145`) uses `emitVector=dir`. Its `circle` layer sprays 30 particles at 8–32 elmo/frame at `emitRot=165` (back along the incoming path). `bigcircle` moves along the dive and `tracers` ring at 80°.
- Remake: `trigger_delayed_hit` queues `ExplosionEvent { up: Vec3::Y, .. }` (`weapon_fx/tick.rs:875-882`), and `spawn_pending_explosions` passes `event.up` as the CEG dir (`spawn.rs:365-370`). Every projectile and laser-bolt impact CEG is therefore aimed at +Y.
- In play: the Pointer missile dives onto its target. Upstream its yellow/red spray shoots back up out of the impact. The remake fires it at 165° from +Y, straight into the terrain, where it is hidden. Its 128-elmo white flash rises instead of sinking.
- Fix: carry the projectile's velocity direction in `ExplosionEvent` and use it as `dir` for impact CEGs. Keep `up` for death and emit CEGs.

### 7. Build beams always use the System BuildLaser, whatever weapon the script fires — severity: medium · confidence: verified
- Original: `emit-sfx 2048+n` fires weapon `n` (`UnitScript.cpp`, `SFX_FIRE_WEAPON` branch).
  - Trojan: `emit-sfx 2048 from center` (`scripts/trojan.bos:55`) with `Weapon1=BuildLightning` (`units/trojan.fbi:66`). That is a `largebeamlaser` 15 thick (30 wide), `corethickness .5`, `lightning` (largelaserfalloff) tiles scrolling at 2/s, and `lightningflare` (muzzlefront) start and pulsing flares at 15·2.5 elmos (`corruptionweapons.tdf:504-525`).
  - Socket: con-lasers fire `emit-sfx 2051` (`socket.bos:41-42`), which is weapon 4 `BuildLaserNoEffect` (`socket.fbi:72`; `anydefs_post.lua` keeps the indices) with `explosiongenerator=custom:none` (`retroweapons.tdf:560`).
- Remake: every `SfxKind::BuildBeam` pushes `weapon: WeaponKind::BuildLaser` (`animation/mod.rs:1038`), including `trojan.rs:74` and `socket.rs:166-169`.
- In play: the Hacker builder's thick, scrolling lightning ray looks like the System's thin white beam. A producing Socket sprays `oldskool_build` squares under its con-lasers, which upstream does not.
- Fix: carry the weapon index in `SfxKind::BuildBeam` and use the unit's weapon at that slot. Draw `largebeamlaser` beams through the large-beam path from finding 2. `lightning`/`lightningflare` also need aliases and their bitmaps bundled (largelaserfalloff.tga, muzzlefront.tga).

### 8. Script- and gadget-emitted CEGs replaced or missing — severity: medium · confidence: verified
- DOS moving: `StartMoving` emits `1024 from ground` every 120 ms (`dos.bos:30-37`), which is `custom:corruption_dust`: 4 yellow→red directional spark lines thrown out of the ground piece. The remake emits `SfxKind::Puff` (`animation/units/dos.rs:64-71`), a team-coloured sphere plus ring (`animation/mod.rs:1124-1165` → `spawn.rs:395-435`).
- Gateway building: `BuildFX` emits `network_build` four times, at 0/90/180/270°, plus `network_build1` every 250 ms (`gateway.bos:46-62`): white squares drifting out and up. The remake emits `Puff` (`gateway.rs:63-72`).
- Packet Enter/Dispatch: `network_enter.lua:43-50` and `network_dispatch.lua:57-62` call `Dissolve()`/`Spawned()` (`packet.bos:50-58`). Those emit `network_dissolve` (15 white squares bursting up plus a 24-elmo white ground flash) and `network_spawn`. The remake despawns or spawns silently (`mechanics/network_buffer.rs:238-310`).
- Firewall charged: the gadget calls `Charge()` when the reload is done (`network_reflectorshield.lua:61-64`). It emits `network_charge` (green squares rising along a ±32 cross) every 120 ms (`firewall.bos:23-34`). The remake maps the Firewall to `NoAnim` (`animation/units/mod.rs:40,72`).
- Logic Bomb under construction: emits `oldskool_build` every 30 ms (`logic_bomb.bos:24-32`). `logic_bomb.rs` has no emit.
- In play: Network players never see the dissolve or spawn bursts, nor the "Firewall ready" glow. DOS and Gateway show team-coloured blobs where KP shows yellow sparks and white squares.
- Fix: emit `SfxKind::Ceg` with the FBI `explosiongenerator0/1` in those drivers. Hook `Dissolve`/`Spawned`/`Charge`/`Discharge` from the buffer and Firewall mechanics. `Puff` should not stand in for an `emit-sfx 1024+n`.

### 9. Firewall protection overlay missing — severity: medium · confidence: verified
- Original: `network_reflectorshield.lua:137-168` redraws every protected unit (`protected[u]`, 20·32 frames, about 21 s) with `bitmaps/kpsfx/shieldEffect.png`. It uses additive `GL_SRC_ALPHA, GL_ONE`, colour (.1, 1, .1), and a texgen scrolling 0.03 per draw frame, so a green band pattern slides over the unit.
- Remake: `Protected` is gameplay only (`command_fire.rs:377`). Nothing renders it, and nothing reads `shieldEffect`.
- In play: casting the Firewall gives no visual feedback on which units are protected.
- Fix: draw a scrolling additive green overlay (or a material swap) on `Protected` units in LOS.

### 10. Flow missiles lack the Starburst engine flare — severity: medium · confidence: verified
- Original: `[FlowMissile]` is a `StarburstLauncher` with `texture1` unset (`networkweapons.tdf:126-138`), so `texture1 = sbflaretex` (backup `flare`, `ProjectileDrawer.cpp:235,567-569`). `CStarburstProjectile::Draw` adds a camera-facing quad of half-size 25 tinted `lightRed(255,180,180)`, near-additive, at the missile (`StarburstProjectile.cpp:404-415`). The tracer parts use `explo`, which is black in KP, and are invisible.
- Remake: `spawn_projectile` draws the model, the `flowtrail` smoke and the `network_flowtrail` spikes, with no flare (`spawn.rs:1001-1105`).
- In play: each of the four missiles per Flow salvo should carry a 50-elmo pink-white glow.
- Fix: for StarburstLauncher, add a 25-elmo `flare` billboard at the projectile, tinted (1, .71, .71), additive.

### 11. BeamLaser drawing: no brightness falloff along the beam, caps outward and always drawn — severity: medium · confidence: verified
- Original: `CBeamLaser::FireInternal` sets `startAlpha = (1 - curLength/maxLength)·intensity·255` and `endAlpha = (1 - (curLength+beamLength)/maxLength)·…`, clamped (`Sim/Weapons/BeamLaser.cpp:389-398`, `maxLength` = range). Edge and core colours are scaled by them (`BeamLaserProjectile.cpp:53-68`), and the body quad goes from `…Start` at pos1 to `…End` at pos2 (`:160-171`). The `texture2` caps extend inward along `-ydir` (= +beam dir at the start) and are drawn only within 1000 elmos of the player camera (`:139-190`).
- Remake: the beam quad gets one colour, `[decay^t; 3]` (`tick.rs:225-235`). Caps are pushed outward by `beam_dir·thickness` at any distance (`tick.rs:239-264`).
- In play: Packet (range 250) and Gateway (350) beams fade toward the target upstream; a 200-elmo Packet shot ends at 20 % brightness. The remake draws them uniformly bright. Caps also make every remake beam about one thickness longer at both ends.
- Fix: put `startAlpha`/`endAlpha` on the two ends' vertex colours (non-large BeamLaser only). Draw caps inward and skip them beyond 1000 elmos.

### 12. CSimpleParticleSystem `pos` offsets: Recoil spawns every particle at the explosion point — severity: medium · confidence: verified
- Original: the CEG `pos` writes `CExpGenSpawnable::pos` (`ExpGenSpawnable.cpp:120-126`). `CProjectile::Init` adds the explosion position to it (`Projectile.cpp:126-128`), but `CSimpleParticleSystem::Init` starts each particle at `p.pos = offset`, the explosion position only (`SimpleParticleSystem.cpp:220`; introduced in 3f52613225 in 2013, so it applies to Spring 105 too). Spawners, muzzle flames and spikes do use their `pos`. Upstream also rolls `r` terms once per system instance, not per particle (`ExecuteExplosionCode` runs once per `count`).
- Remake: `spawn_particle_system` adds `frame * pos` per particle, with a fresh roll per particle (`ceg.rs:662-679`).
- In play: `system_nx_fire` (`pos=-30 r60, 1, -30 r60`) spreads the NX fire over a 60×60 field in the remake. Under Recoil the NX burns as one column at the impact point. The `system_nx`/`system_sigterm` blasts (`pos=0,30,0`) start 30 elmos up instead of at ground level. KP authored these offsets for pre-2013 engines, but the reference engine ignores them.
- Fix: for `Particle` layers, start particles at `origin`; keep `pos` for spawners, flames and spikes. If the authored look is wanted on purpose, document it as a deviation.

### 13. Lightning arcs are static and fade, and the Gauss arc is 0.12 s instead of a 12-frame salvo — severity: medium · confidence: verified
- Original: `drawArc` re-rolls every vertex's spray and width direction on every `DrawWorld` call (`network_arceffect.lua:100-114,141-151`), so the strip crackles every render frame. The colour is constant, with no fade (`:116-133`). `GaussCannon` has `beamtime=.4` and no `beamburst` (`networkweapons.tdf:89-90`), so `CBeamLaser::Init` makes a 12-shot, 0-delay salvo (`BeamLaser.cpp:104-111`). Each shot's explosion calls `BigArc` with `liveUntil = frame+1` (`network_arceffect.lua:81-86,43`), which gives about 0.4 s of flicker.
- Remake: the jitter is rolled once at spawn (`spawn.rs:1240-1259`). Alpha fades with remaining life (`tick.rs:161-164`). `ARC_LIFETIME = 0.12` is justified as "upstream redraws for exactly one sim frame" (`spawn.rs:1277-1280`), which misses the salvo. The width is a constant camera-facing ribbon.
- In play: the Connection's bolt is a brief, smooth, fading arch instead of a 0.4 s flickering electric strand. The Gateway build strand doesn't crackle either.
- Fix: re-jitter the polyline every tick (or render frame) and drop the alpha fade. Hold the Gauss arc for `int(beamtime·30)` frames, ideally re-aimed per frame.

### 14. Geovent puffs: invented glyphs, colour and fade instead of KP's smoke textures — severity: medium · confidence: verified
- Original: geothermal features emit `CGeoThermSmokeProjectile(…, 6, 0.35, 0.8)` every frame (`Sim/Features/Feature.cpp:723-731`). That is a `CSmokeProjectile` with a random smoke texture (`SmokeProjectile.cpp:66`) and colour `0.8·(1-age)` premultiplied with alpha `(1-age)` (`:100-124`). KP's `[smoke]` table (`RESOURCES.TDF:5-19`) holds 12 32×32 tiles, each with a pair of bright-green (0,255,0) pixel binary digits in one corner (`bitmaps/smoke/*.tga`).
- Remake: two hand-made 8×8 "0"/"1" glyphs (`terrain/geovent.rs:179-206`) in additive linear green (0.05, .35, .10). No alpha fade; instead "the fade is baked into quad size" (`:504-511`). They emit at 18 Hz ("KP slows that down", `:103-107`; the engine doesn't).
- In play: every datavent on every map streams dim, centred single digits that shrink away, instead of brighter digit pairs placed off-centre that fade out while growing.
- Fix: bundle `bitmaps/smoke/smoke00-11.tga`, pick one at random per puff, colour `0.8·(1-age)` with alpha `(1-age)` premultiplied, half-size = `size`. The rate change can stay as a documented choice.

### 15. Laser and beam materials premultiply by texture alpha; the engine does not — severity: medium · confidence: verified
- Original: lasers and beams use vertex alpha 1/255 (`LaserProjectile.cpp:206-218`, `BeamLaserProjectile.cpp:56-68`), drawn with `BlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA)` and discarding alpha 0 (`ProjectileDrawer.cpp:804,831`). Any texel with alpha > 0 therefore adds its full `tex.rgb·col`.
- Remake: `BeamMaterialCache` uses `AlphaMode::Add` (`weapon_fx/shared.rs:557-561`), which Bevy turns into `rgb·a` (`bevy_pbr/src/render/pbr_functions.wesl:936-948`). CEG particles correctly use `Premultiplied` (`ceg.rs:126-130`).
- In play: `bytemegabeammid.tga` has every texel at alpha ≈142/255, so the Byte's MegaBeam body (and its `bytemegabeam` caps) draws at about 56 % of upstream brightness. `arrow.tga` has 5576 of its 9244 visible texels semi-transparent, so the Bit's chevrons lose their soft bright edges.
- Fix: draw beam and bolt quads with `AlphaMode::Premultiplied` and vertex alpha ≈ 0 (or 1/255), as the engine does.

### 16. Effect colours go through linear HDR + TonyMcMapface (+ optional bloom) — severity: medium · confidence: verified (facts) / likely (magnitude)
- Original: 8-bit gamma-space framebuffer, colours clamped. TDF `rgbColor` and colorMap values are gamma-space bytes.
- Remake: the camera has `Hdr` and no `Tonemapping` override (`rendering/camera.rs:146`), so it gets Bevy's default `TonyMcMapface` (`bevy_render/src/view/mod.rs:312-325`), plus `Bloom` when enabled (`rendering/settings.rs:261-266`). Effect colours are fed as linear values: `Color::LinearRgba(color)` in `shared.rs:572-576`, raw colorMap floats on the vertices. Textures are `Rgba8UnormSrgb` (`assets/meshes.rs:446`).
- In play: mid-tone effect colours (DOS 0.5 green, colorMap .2–.5 values, smoke 0.65) render lighter. Additive stacks roll off instead of clipping, and saturated neon primaries desaturate. This is pipeline-wide; coordinate with the rendering audit.
- Fix: decode TDF/CEG colours as sRGB (`Color::srgb`), and consider `Tonemapping::None` (or a clamp) for the fx batches.

### 17. Gadget `Explosion` callins that suppress CEGs are not modelled — severity: low · confidence: verified
- Original: a synced gadget returning `true` sets `noGfx` and skips the CEG (`Game/GameHelper.cpp:269,320`; `Lua/LuaHandle.cpp:2253-2300`). `Launcher.lua:26-32` returns `true` for every MineLauncher hit whose owner exists, so there is no `corruption_shot1` there; it spawns the Logic Bomb instead. `areadenial.lua:40-50` returns `true` (no CEG and no zone) for the NX and Infection when the shooter is gone.
- Remake: `trigger_delayed_hit` always queues the weapon CEG, the NX zone and the mine (`tick.rs:834-882`).
- In play: Mine Launcher hits show a dark-red puff that KP doesn't show. An NX shell whose Pointer died in flight still lands with its 64 s fire field and zone.
- Fix: apply both callins' conditions when the delayed hit fires.

### 18. Worm bite: one shockwave at the target at fire time, instead of two after the lunge — severity: low · confidence: verified
- Original: `FireWeapon1` lunges the head 112 elmos (`wait-for-move head`, ≈0.33 s), then `emit-sfx 4097 from head` and `from end` (`worm.bos:186-200`). These are two `SFX_DETONATE_WEAPON` explosions at the piece positions, so two `corruption_worm_splash` rings (`UnitScript.cpp:800-819`).
- Remake: a single CEG at `event.target_pos` when the shot is queued (`spawn.rs:134-163`); the damage already uses both points (`mechanics/worm.rs:150-166`).
- Fix: emit the splash CEG from the head and end pieces when the head move completes.

### 19. Absent visual tags read as zero instead of the engine defaults — severity: low · confidence: verified
- Original: `coreThickness` defaults to 0.25 and `thickness` to 2.0 (`WeaponDef.cpp:261-262`). A missing `rgbColor` on non-Cannon weapons becomes `hs2rgb(0)` = red (`springcontent/gamedata/weapondefs_post.lua:141-150`).
- Remake: `core_thickness: s.f32("corethickness")` and `thickness: s.f32("thickness")` are 0 when absent (`spring-tdf/src/weapon.rs:564-565`). The core is skipped below 0.01 (`spawn.rs:767-768`), thickness becomes `max(1)` (`:665`), and the missing colour becomes white (`weapon.rs:508-512`).
- In play: Packet and Gateway beams lack their thin white core line, and the BuildLaser lacks its brighter core and flare core. The MineLauncher bolt's end caps are white and 1 elmo instead of red and 2 elmos. `docs/script-and-weapon-parity.md` even states the 0.25 default.
- Fix: `f32_or("corethickness", 0.25)`, `f32_or("thickness", 2.0)`, and the hs2rgb(0) default in `resolved_rgb`.

### 20. CEG parser mis-reads `r 60` (a space after the op), and the TDF check can't catch it — severity: low · confidence: verified
- Original: `ParseExplosionCode` reads the operand with `strtod`, which skips leading spaces (`ExplosionGenerator.cpp:902`). `system_sigterm_fire/pillar pos=-30 r 60, -35.0, -30 r 60` (`system_sigterm.tdf:108`) is therefore `-30 + rand·60` per axis.
- Remake: `parse_float` requires a digit right after the op (`spring-tdf/src/explosion.rs:105-121,128-160`), so `r` is dropped and `60` added: `pos: vec3(30.0, -35.0, 30.0)` (`ceg_defs/system_sigterm.rs:170`). `every_source_def_matches_the_parsed_tdf` (`content/cegs.rs:365`) compares the statics against that same parser, so it passes.
- In play: the SIGTERM's dark pillar squares all sit at one corner offset (+30, +30). Under finding 12's Recoil semantics they should be centred.
- Fix: skip whitespace before an operand in `parse_float` and regenerate the defs.

### 21. CEG runtime details: drag and gravity applied before the move; spawn conditions ignored — severity: low · confidence: verified
- Original: `CSimpleParticleSystem::Update` does `pos += speed; speed += gravity; speed *= airdrag` (`SimpleParticleSystem.cpp:183-189`). `CCustomExplosionGenerator::Explosion` spawns a layer only if its flags match `GetFlagsFromHeight`: nothing at all more than 20 elmos underground or underwater, and the ground flash only for ground explosions (`ExplosionGenerator.cpp:94-129,1068-1112`).
- Remake: drag, then gravity, then move (`ceg.rs:1029-1037`), so total travel is shorter by one drag factor (×0.7–0.9: `oldskool` tracers 30 %, squareclouds 20 % tighter). `spawn_ceg` ignores `EffectLayer.air/ground/water` and always adds the ground flash on the terrain under the blast (`ceg.rs:474-541`).
- Fix: move before drag; evaluate the height flags before spawning.

### 22. Cannon shells with `texture1=black` drawn as small black spheres — severity: low · confidence: verified
- Original: `black` is not in KP's atlas (`RESOURCES.TDF:49-101`), so it maps to `DefaultAtlasTexture` (`TextureAtlas.cpp:354-361`), which is invalid (`Projectile.cpp:238-242`), and `CExplosiveProjectile::Draw` returns (`ExplosiveProjectile.cpp:88-89`). Only the `cegTag` trail is visible.
- Remake: an opaque 0.02-grey sphere of radius 1.5 (`spawn.rs:971-977,1040-1050`); the comment claims KP draws "an opaque near-black ball". The remake's `CegTexture::Black` alias (`ceg_def.rs:449-450`) doesn't exist upstream either.
- In play: a dark dot leads every Exploit `corruption_BCtrail`.
- Fix: draw nothing for model-less Cannon shells whose texture is unresolved.

## Doc drift
- `FEATURES.md:349-350`: "Each shot produces a muzzle flash sprite". The code adds muzzle CEGs only for Bit, Byte and Pointer (`animation/mod.rs:177-183`).
- `FEATURES.md:357-358`: "PacketBeam render as a fan of thin segments". The code draws one beam per shot.
- `FEATURES.md:359-361`: "spheres or small cubes … Pointer has a red trail". The code uses S3O models (octashot, sigterm, network_medium_missile) and `texture2` trails.
- `FEATURES.md:362-363`: "Wormbite flash with a short orange burst". The code plays `corruption_worm_splash` (`spawn.rs:143-163`).
- `FEATURES.md:364-365`: "Each impact pops a colored burst … AoE-scaled". Impacts are CEG-driven; the generic burst only covers missing generators, which no KP weapon has.
- `FEATURES.md:416-419`: describes the per-piece faction-coloured bursts as if they matched the original (finding 4).
- `FEATURES.md:458-460`: "Hole / Window / Port: a single nano-emitter. Connection: a placeholder ray". Window, Port and Connection emit nothing (`animation/units/mod.rs:72`, `window.rs`, `connection.rs`).
- `FEATURES.md:664`: DOS "leaves a long visible particle trail". The code leaves team-coloured puffs (finding 8).
- `FEATURES.md:50-51`: "no fog-of-war hides any region" contradicts the unit fog in `mechanics/cloak.rs` (and `FEATURES.md:245`).
- `docs/script-and-weapon-parity.md:184-186`: "LaunchMines … mines spawn directly". `command_fire.rs:1027-1037` fires real MineLauncher bolts.
- `docs/script-and-weapon-parity.md` (Build lasers): "corethickness default 0.25". The code doesn't apply it (finding 19).
- `command_fire.rs:1623-1626`: "Infection's gas has no CEG upstream" is wrong (`corruptionweapons.tdf:417`).
- `animation/units/mod.rs:40`: Firewall "drives nothing we render". Its `Charge()` emits `network_charge`.
- `animation/units/carrier.rs:3`: "carries the host-driven build beam". No such beam exists.
- `ceg_def.rs:392-399` (`CegTexture` docs): claims every alias the weapon TDFs reach. It misses `lightning`/`lightningflare` (BuildLightning) and `smoketrail` (default missile trail), and adds `black`, which KP's table lacks. `unit-bundle.md`: "the three engine bitmaps the effects reference" leaves out largelaserfalloff/muzzlefront.

## Checked and matching (one line each)
- Smoke trails (`CSmokeTrailProjectile`): smokeTime 60, half-width `1+t·7`, colour 0.65, fade `(1-t)(0.7+|dif·dir|)`, hidden first and last segment, premultiplied blend, `pointertrail`/`firetrail`/`flowtrail` aliases.
- `CBitmapMuzzleFlame`: eased growth `size·(1+g·(1-(1-life)²))`, fixedSideDir planes, frontOffset, front and side quad layout and UVs.
- `explspike`: `lengthGrowth = |dir|·(0.5+0.4r)`, `dir /= lengthGrowth`, additive `laserend` coloured `alpha·color`, alphaDecay per frame.
- `CStandardGroundFlash`: ring `circleGrowth·(age+1)` with linear alpha, flash ×10 ramp to 0.091 then linear, `SRC_ALPHA,ONE`, ground+1, `CalcNormal` plane, KP `solidwhite`/`hollowsquare` textures.
- `CExpGenSpawner`: `delay=d i k` per count index, child damage from `damage=0 i1`, child pos and dir from the spawner.
- CEG ops `r/i/d/y/x`, `numParticles` int truncation (`corruption_builder` `0 r2`), `count=0` (network_flowtrail Circle), colorMap even-stop interpolation, one-sided spreads `base + r·spread`.
- CEG statics spot-checked field by field against TDF: `oldskool`, `corruption_worm_splash`, `system_sigterm` (only the parse bug of finding 20), `network_flowtrail` (`//` comment stripped).
- LaserCannon bolts: `maxLength = duration·velocity`, U along lead→tail with the growth offset, outward `texture2` caps, bytelaser caps on the outer bolt.
- BeamLaser `flare` half-size `thickness·laserflaresize` (Packet 30, Gateway 45, BuildLaser 75), `beamdecay` per frame and `beamTTL` lifetimes for Packet and Gateway.
- GaussCannon `intensity=0` gives an invisible engine beam; the arc starts from piece `correcter`, with arch 160, spray 15/5, width 4/2, green with 0.2–0.3 red/blue.
- Weapon colours: rgbColor 0-255/0-1 normalisation; `color=40` hs2rgb; `laserfalloff`/`laserend` defaults for untextured beams.
- Death debris: FALL launch `(±3, 1.2..6.2, ±3)` plus compressed unit speed, drag 0.997, spin, bounce chance 0.666; SHATTER into ≤10 chunks, BITMAPONLY. No KP `pieceExplosionGenerators`, so no CEG trails.
- Alpha-0 texel discard emulated by zeroing RGB (`assets/meshes.rs:324-334`).
- KP's `explo`/`explofade`/`heatcloud`/`flame` = black: no default-explosion visuals; `useDefaultExplosions` unused; KP scars transparent.
- Muzzle CEGs: Bit `oldskool_shot2`, Byte/Pointer `oldskool_shot1`; Hole `corruption_const` per arm stroke; Obelisk `corruption_charge` every 50 ms while not reloading.
- Seismic pings (`weapon_fx/seismic.rs`): Worm detector, signature growth, radar error, LOS gating.
- Camera shake: KP's `camera_shake.lua` is `enabled=false`, so neither game shakes.

## Unverified leads
- Emit-sfx CEGs take the unit's up vector as `dir` (`animation/mod.rs:1149-1154`), not the piece's emit direction (`UnitScript.cpp` `EmitSfx` → `absDir`). Harmless today; it matters once `network_build`/`corruption_dust` (`emitVector=dir`) are restored per finding 8. Settle by comparing against `GetEmitDirPos` for the Gateway's `center` and the DOS's `ground`.
- CEG literal vectors and offsets are rotated by the emitter's up vector (`ceg.rs:470-472`, "turn with it"). The engine uses world space. It only shows on slopes; questionable as a deliberate choice.
- Depth ordering: each material's quads form one mesh entity at the origin, so Bevy orders premultiplied trails against particles by material, not distance. Upstream sorts per projectile. Needs a render capture to see whether wrong overlaps are visible.
- Flame and particle first/last frames: CBitmapMuzzleFlame `ttl` is an int (`12 r4` truncates) and the flame draws through life = 1.0. The remake stops at `(ttl-1)/ttl`, about 2 frames short on the 5-frame worm splash.
- The Signal is not a unit in the remake, so it can't be shot down. Upstream its `Killed()` still drops the SIGTERM (`signal.bos` `emit-sfx 2048 from bomb`). The `FireFlash` placeholder (`signal.rs:94`) is dead code. Gameplay.
- Documented deliberate deviations worth a second look: the BuildLaser/DOS sqrt thickness shrink (`tick.rs:199-216`); the 18 Hz geovent rate; the Hacker/Network alpha fade instead of the alpha-threshold dissolve; the `network_build.lua` scrolling band overlay (both listed in `script-and-weapon-parity.md`).
