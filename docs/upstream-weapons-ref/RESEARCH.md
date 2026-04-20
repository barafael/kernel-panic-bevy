# Upstream Kernel-Panic Weapons Research — System Faction

Research scope: every weapon, projectile visual, beam animation, explosion, and
supporting asset used by the **System** (CPU/green) faction in the original
Spring/Recoil mod under `upstream/Kernel-Panic/`. The goal is to drive a
port that uses real upstream assets & numbers, not ad-hoc re-implementations.

> Video reference: `C:/Users/rbachmann/Videos/Captures/kp-weapon-anims.mp4`
> (30 fps, 50 s, 3840×2088). Key frames live in
> [`frames/`](frames/):
> - **Pointer** (first unit shown, 0–5 s + 10–13 s):
>   [`pointer_scene_opening.jpg`](frames/pointer_scene_opening.jpg) (multiple
>   pointers firing over the battlefield + red impact),
>   [`pointer_full_arc_wideview.jpg`](frames/pointer_full_arc_wideview.jpg)
>   (complete red-orange ballistic arc with magenta head + white-tracer
>   impact at far end),
>   [`pointer_impact_big_red.jpg`](frames/pointer_impact_big_red.jpg) (huge
>   red `oldskool_impact` particle burst mid-explosion),
>   [`pointer_impact_from_behind.jpg`](frames/pointer_impact_from_behind.jpg),
>   [`pointer_projectile_flight.jpg`](frames/pointer_projectile_flight.jpg)
>   (magenta octashot head, early flight, short trail),
>   [`pointer_projectile_arc.jpg`](frames/pointer_projectile_arc.jpg)
>   (projectile near apex, full arc visible).
> - **Bit**: [`bit_beam_closeup.jpg`](frames/bit_beam_closeup.jpg) (clean
>   cyan chevron-arrow bolts with pale lavender impact hemispheres).
> - **Byte**: [`byte_beam_firing.jpg`](frames/byte_beam_firing.jpg)
>   (MegaBeam — magenta fat bolt, white core),
>   [`byte_impact_oldskool.jpg`](frames/byte_impact_oldskool.jpg) +
>   [`byte_impact_byte_firing.jpg`](frames/byte_impact_byte_firing.jpg)
>   (yellow/orange block-cloud impact with white radial `hline` tracers).

> Assets copied & converted alongside this doc:
> - every `.tga` → `.png` (viewable with any image tool, see §7)
> - every `.s3o` → `.obj` via [`s3o_to_obj.py`](s3o_to_obj.py) (viewable in
>   Blender / MeshLab / VS Code .obj preview)

---

## 0. TL;DR — where the weapon colors actually come from

Sampling texture pixels (see §7 table, augmented with max-channel RGB below)
together with video frame inspection resolves the colors decisively:

| Weapon                 | Visible color in video     | Source                                                                |
| ---------------------- | -------------------------- | --------------------------------------------------------------------- |
| bit `Line` bolt        | bright cyan arrow-chain    | `arrow.tga` is baked cyan (max 0,255,255). `corethickness=1` → core=full-width white × cyan texture wins. The `RGBcolor 255 128 128` tag is dead weight. |
| byte `MegaBeam` bolt   | magenta/pink with white core | `bytemegabeam.tga` / `bytemegabeammid.tga` are baked magenta (max 255,89,254). Core pass × white = hot pink; halo pass × `RGBcolor 127 255 127` = faint olive-pink around edge. |
| pointer `Geometric` head | tiny magenta projectile    | `octashot.tga` (8×8) is magenta (max 192,0,192); `octashot.s3o` renders flat-shaded.                     |
| pointer smoke trail    | thick orange-red ribbon    | `pointershottrail.tga` is baked orange-salmon (max 255,110,31).       |
| pointer NX smoke trail | thin cyan-yellow ribbon    | `firetrail.tga` is yellow-tinted but almost transparent (α≈3/255).    |
| bit muzzle flare       | cyan 4-point star          | `arrowflare.tga` baked cyan (max 0,255,255) — see [`arrowflare.png`](arrowflare.png). |
| bit blue impact puffs  | pale lavender hemispheres  | `oldskool_shot1` CEG, `whitecircle.tga` + colormap white→blue.        |
| byte impact cloud      | yellow-orange block cloud  | `oldskool` CEG, `solidwhite` square + yellow→red→black colormap + white `hline` radial tracers. |
| pointer impact burst   | big white flash + orange   | `oldskool_impact` CEG — white flash ring (size 128), orange sparks, white tracers. |
| retro death streaks    | thin orange radial beams   | `RetroDeath*` weapons with `color=40` palette HSV ≈ orange-yellow (see `hs2rgb`). |

**Implication for the port:** don't try to reconstruct colors from the
weapon TDF alone — you will get the wrong colors. The textures are the
authoritative source; RGBcolor/rgbColor2 are only edge tints and often
entirely hidden by the core pass. Ship the upstream `.tga`/`.png` files
as-is.

---

## 1. Faction roster (SIDE0 = "System", commander = kernel)

From [upstream/Kernel-Panic/gamedata/SIDEDATA.TDF](../../upstream/Kernel-Panic/gamedata/SIDEDATA.TDF):

| Role       | Unit         | Object model          | Side |
| ---------- | ------------ | --------------------- | ---- |
| Homebase   | `kernel`     | `kernel.s3o`          | CPU  |
| Arty       | `pointer`    | `cube.s3o`            | CPU  |
| Spam       | `bit`        | `ball.s3o`            | CPU  |
| Heavy      | `byte`       | `octaeder.s3o`        | CPU  |
| Builder    | `assembler`  | `assembler.s3o`       | CPU  |
| Minifac    | `socket`     | `socket.s3o`          | CPU  |
| Airstrike  | `terminal`   | `terminal.s3o`        | CPU  |
| Corpse     | `badblock`   | `badblock.s3o`        | CPU  |
| Mine       | `logic_bomb` | `logic_bomb.s3o`      | CPU  |
| Minesweeper| `mineblaster`| `nullobject.s3o`      | CPU  |

Ability‐bearing combat units are **pointer / bit / byte**, plus `terminal`'s
airstrike-spawned `signal` bomber and the `byte`'s mine-launcher secondary.

---

## 2. Weapon-type remapping — how legacy TDF tags resolve

All `weapons/*.tdf` files use the old OTA/legacy tag style. At load time the
engine runs [cont/base/springcontent/gamedata/weapondefs_post.lua](../../upstream/RecoilEngine/cont/base/springcontent/gamedata/weapondefs_post.lua#L82-L121)
which maps them onto modern `weaponType` strings:

```
dropped=1                           → AircraftBomb
vlaunch=1                           → StarburstLauncher
beamlaser=1                         → BeamLaser   (instant hitscan ribbon)
isshield=1                          → Shield
waterweapon=1                       → TorpedoLauncher
lineofsight=1 + rendertype=7        → LightningCannon
lineofsight=1 + beamweapon=1        → LaserCannon (slow bolt, finite length!)
lineofsight=1 + smoketrail=1        → MissileLauncher
lineofsight=1 + rendertype=5        → Flame
else                                → Cannon
```

**Key takeaway:** upstream KP never uses `beamlaser=1`. The "beams" you see
for **bit** and **byte** are actually `LaserCannon` projectiles — finite-length
bolts that travel at `weaponvelocity` (not hitscan). The `duration` tag on a
LaserCannon sets the visual bolt length as a fraction of one second of travel:
`maxLength = duration * weaponvelocity`. The bolt extends from 0 to maxLength
on fire, then contracts to 0 on impact.

The **pointer**'s `Geometric` weapon becomes a `MissileLauncher` (because
`smoketrail=1` is set) that renders the `octashot.s3o` 3D model with a smoke
ribbon trailing behind.

The **MineLauncher** on byte is explicit `WeaponType=LaserCannon` with
`ballistic=1 myGravity=.4` — a gravity-affected cannon-ish bolt rendering the
`black.tga` sprite.

The `SigTerm` terminal bomb is explicit `WeaponType=AircraftBomb`, model
`sigterm.s3o`, dropped by the `signal` bomber.

---

## 3. Unit weapons in detail

### 3.1 pointer — the "red artillery"

`units/pointer.fbi`:
- `Weapon1 = Geometric`, `Weapon2 = nx` (slave; commandfire NX-flag dgun).
- Script [scripts/pointer.bos](../../upstream/Kernel-Panic/scripts/pointer.bos):
  pieces `base/body/left/right/gun/gunbase/gunpoint`. Aim emits sfx 1024
  from `gunpoint`. `Close/Open` anims expose the gun when stopped;
  StartMoving retracts it and spins body around x-axis at 180 deg/s.
  AimWeapon1 turns `gunbase` on x-axis, body heading via `set HEADING` loop
  with +/- 270 deg/s TURNRATE cap.

#### `Geometric` weapon → MissileLauncher

`weapons/retroweapons.tdf` `[Geometric]`:
```
model=octashot.s3o
texture1=black         # (unused for a MissileLauncher 3D-model projectile)
texture2=pointertrail  # smoke-trail ribbon texture (pointershottrail.tga)
size=5
soundstart=pointerfire
soundhit=pointerhit

range=1400
weaponvelocity=400  startvelocity=400
smoketrail=1
trajectoryheight=1   # arcing flight
tracks=1 turnrate=20000
weapontimer=5
reloadtime=4
areaofeffect=32
damage.default=4000
explosiongenerator=custom:oldskool_impact
```

**Visual** (confirmed in frames [`pointer_projectile_arc.jpg`](frames/pointer_projectile_arc.jpg),
[`pointer_projectile_flight.jpg`](frames/pointer_projectile_flight.jpg)): the
projectile is a tiny magenta/pink head (the `octashot.s3o` model rendered with
its magenta `octashot.tga` texture, RGB ≈ 192,0,192) dragging a long thick
orange-red smoke ribbon behind it. The ribbon texture `pointershottrail.tga`
has max channel (255, 110, 31) — an orange/salmon palette, NOT a neutral
smoke puff — so even before CEGs the trail is very red-orange. The
trajectory arcs high (`trajectoryheight=1` → arc apex ≈ distance) and homes
onto the target. Ground impact ([`pointer_impact_oldskool_impact.jpg`](frames/pointer_impact_oldskool_impact.jpg))
is the `oldskool_impact` CEG: large yellow/orange particle cloud, a big
white flash ring and radial `hline` tracers. See §4.

Firing sfx on the unit: `emit-sfx 1024 from gunpoint` in `FireWeapon1()`.
Sound IDs ≥1024 are `SFX_CEG` — they play the indexed `explosiongenerator#`
from the unit FBI's `[SFXTypes]`. Pointer's `[SFXTypes]` sets
`explosiongenerator0=custom:oldskool_shot1` — the muzzle flash
(see §4, `oldskool_shot1`, a soft blue-white circle puff).

#### `nx` weapon → MissileLauncher (command-fired dgun)

```
model=octashot.s3o
texture1=black
Texture2=firetrail      # firetrail.tga instead of pointertrail
smoketrail=1
trajectoryheight=1  turnrate=20480
reloadtime=30  commandfire=1
areaofeffect=240
explosiongenerator=custom:system_nx
damage.default=200
```

NX Flag is Pointer's special attack (gadget
[specialattack.lua](../../upstream/Kernel-Panic/LuaRules/Gadgets/specialattack.lua),
CMD_NX id). Firing it produces a big, long-burning fire zone at the impact
point (240-elmo AoE). `system_nx` explosion spawner is detailed in §4.

### 3.2 bit — the "bright blue" finite-length laser bolt

`units/bit.fbi`: `Weapon1 = Line`, ball.s3o model, very cheap spam.

Script [scripts/bit.bos](../../upstream/Kernel-Panic/scripts/bit.bos): almost
trivial. `AimWeapon1` turns `gunbase` to h/p instantly.
`FireWeapon1 { emit-sfx 1025 from gunpoint; }` — 1025 = explosiongenerator1 =
`oldskool_shot2`, the yellow/white `arrowflare` muzzle flare. Note the FBI
registers **two** muzzle flashes: `shot1` (generator 0, emitted elsewhere if
any) and `shot2` (generator 1, the one actually fired here).

#### `Line` weapon → LaserCannon

```
beamweapon=1 lineofsight=1           # → LaserCannon (NOT a BeamLaser!)
RGBcolor=255 128 128                 # edge color — salmon-pink on paper
duration=0.2                         # bolt visual length = 0.2*512 = 102.4 elmos
thickness=4  corethickness=1         # core = inner ribbon (white by default rgbcolor2)
texture1=arrow                       # 256x64 arrow beam
texture2=none                        # no end-caps
soundstart=bitfire
weaponvelocity=512  range=256
reloadtime=0.5
areaofeffect=8
damage.default=80
explosiongenerator=custom:oldskool_shot1   # (CEG run each frame from Update())
```

Rendering: `CLaserProjectile::Draw` draws the bolt as a textured ribbon
between `drawPos` (head) and `drawPos - dir*curLength` (tail). Two passes
overdrawn: outer quad at `thickness` with `color` (`RGBcolor`), inner quad
at `thickness*corethickness` with `color2` (`rgbColor2`, defaults to white).
End-cap texture optional (`texture2`); here set to "none" so only the
stretched `arrow.tga` main ribbon is drawn.

**Why the bolt is bright cyan, not salmon** (confirmed in
[`bit_beam_closeup.jpg`](frames/bit_beam_closeup.jpg)): the `arrow.tga`
texture is **baked bright cyan** (max RGB = (0, 255, 255); see
[`arrow.png`](arrow.png)), and with `corethickness=1` the inner-core quad
(multiplied by white `rgbColor2`) completely covers the outer quad (tinted
by RGBcolor=255,128,128). So what you see is `arrow_cyan × white` = pure
cyan chevron arrows. The `oldskool_shot1` CEG dots alongside are the pale
blue hemispheres (`whitecircle` texture with the blue-to-white colorMap).
The RGBcolor tag is effectively unused here.

In-frame the "beam" is actually a **chain of chevron arrows** — `arrow.tga`
contains 4 `«««<` arrow heads, and the texture tiles/stretches along the
beam, so a single bolt visually reads like ~4 staggered cyan chevrons
pointing toward the target. `texture2=none` means there are no rounded end
caps — the beam literally terminates at the tip of the final chevron.

### 3.3 byte — the "purple elongated bursts"

`units/byte.fbi`: `Weapon1 = MegaBeam`, `Weapon2 = MineLauncher`.

Script [scripts/byte.bos](../../upstream/Kernel-Panic/scripts/byte.bos) has
**four firing points** `bp0 … bp3`. `FireWeapon1()` cycles the emission
piece:
```
emit-sfx 1024 from bp0;  sleep 90;  gp=1;  sleep 150;
emit-sfx 1024 from bp1;  sleep 90;  gp=2;  sleep 150;
emit-sfx 1024 from bp2;  sleep 90;  gp=3;  sleep 150;
emit-sfx 1024 from bp3;  sleep 90;  gp=0;
```
`QueryWeapon1()` returns `bp{gp}` so each of the 4 bolts in a volley spawns
from a different piece. Combined with the weapon's `burst=4 burstrate=0.25`
this produces **four rapid bolts per salvo, one per corner/spike of the
octaeder**. `Open()`/`Close()` animates blade rotors and a base lift.

#### `MegaBeam` weapon → LaserCannon

```
beamweapon=1 lineofsight=1           # → LaserCannon
RGBcolor=127 255 127                 # edge color — muted green
thickness=16  corethickness=0.5      # half-thickness white-hot core
duration=0.05                        # bolt visual length = 0.05*1024 = 51.2 elmos
texture1=bytelasermid                # 32x32, the stretched middle
texture2=bytelaser                   # 32x32, end caps
soundstart=bytefire  soundhit=bytehit
weaponvelocity=1024  startvelocity=1024
sprayangle=1024                      # spread across the 4 volleys
range=512
reloadtime=2  burst=4  burstrate=0.25
areaofeffect=128                     # BIG crater
damage.default=200
explosiongenerator=custom:oldskool   # (§4 — the "substantial" ground explosion)
```

**Visual** (confirmed in [`byte_beam_firing.jpg`](frames/byte_beam_firing.jpg),
[`byte_impact_byte_firing.jpg`](frames/byte_impact_byte_firing.jpg)): short
fat ribbons (51 elmos long) that read **magenta/pink with a bright white
hot-core**, not green. Same color-source story as bit:
`bytemegabeam.tga` and `bytemegabeammid.tga` are baked magenta
(max RGB ≈ (255, 89, 254); see [`bytemegabeam.png`](bytemegabeam.png)).
The core-pass multiplies against white, producing a bright
white-to-magenta bolt; the halo-pass multiplies against `RGBcolor=127,255,
127`, slightly tinting the edges toward olive-pink (nearly indistinguishable
against the fat pink core).

Because `burst=4 burstrate=0.25` at `sprayangle=1024` (1024 TAANG units =
±22.5° cone), the byte fires 4 closely-spaced bolts in ≈1 s that fan out;
the "elongated" look comes from the 51-elmo ribbons plus end-cap quads
stretched with `bytelaser.tga`. Bolts live only ≈1.5 frames (duration=0.05)
so each one is a flash, not a sustained beam.

Ground impact: `oldskool` → 15 squarecloud red/yellow particles + 15
hline tracers + 16-flashSize groundflash (see §4). With AoE 128 and 4
bolts per volley landing in a sprayed cone, the impact is the big yellow
flash you see in the video.

#### `MineLauncher` weapon → LaserCannon (ballistic)

```
WeaponType=LaserCannon                # explicit
ballistic=1  myGravity=.4
texture1=black
sprayangle=1000
range=1100
weaponvelocity=200
reloadtime=2.2
areaofeffect=30
damage.default=3
explosiongenerator=custom:corruption_shot1
cegTag=minelauncher                   # CEG while flying (hex-star trail)
```

Fired via CMD_MINELAUNCHER gadget (see
[specialattack.lua](../../upstream/Kernel-Panic/LuaRules/Gadgets/specialattack.lua),
cob-script `LaunchMines`). The projectile leaves a `minelauncher` CEG trail
(hex-star particle, `hexastar.tga`, color-shifting blue→orange→crimson).

### 3.4 terminal + signal — SIGTERM airstrike

- `terminal` itself only has `BuildLaser` + shields.
- [airstrike.lua](../../upstream/Kernel-Panic/LuaRules/Gadgets/airstrike.lua)
  registers CMD_AIRSTRIKE on terminal; target click spawns a `signal` unit
  (VTOL) flying the `SigTerm` AircraftBomb.
- `signal.fbi` uses `[SFXTypes] explosiongenerator0=custom:oldskool_shot1,
  explosiongenerator1=custom:oldskool_shot2`.
- `SigTerm` weapon:
  - `WeaponType=AircraftBomb  model=sigterm.s3o  areaofeffect=900`
  - `explosiongenerator=custom:system_sigterm` — the enormous fire pillar.

### 3.5 Assembler / Kernel / Socket / Terminal / Badblock

All use `Weapon1=BuildLaser` (+ shields on factories). BuildLaser is the
construction beam. Not needed for combat visuals but relevant for reference:

```
[BuildLaser]
beamlaser=1       # real instant hitscan beam → BeamLaser
beamtime=0.06
beamTTL=2
RGBcolor=255 255 255
intensity=5  thickness=5
range=256
burst=30 burstrate=0.01  reloadtime=2
damage.default=0.0000000001
```

Kernel's death is `ExplodeAs=RetroDeathVBig` (huge radial splatter of retro
beams + `oldskool_vbig` ground cloud, flashSize 96).

### 3.6 Death / self-destruct weapons (RetroDeath family)

Every System unit dies via a `RetroDeath*` explosion — these are themselves
weapons (`beamweapon=1 → LaserCannon`) with high `sprayangle=1536` (about
33.75°). On death, the engine fires the weapon in all directions:
- `RetroDeath`      — default, `soundstart=bitdeath`, `oldskool_death` CEG.
- `RetroDeath_pointer` — uses `pointerdeath` sound.
- `RetroDeathBig`   — bytes + assembler, `oldskool_big`.
- `RetroDeathVBig`  — homebase (`kernel`), `oldskool_vbig` with AoE 384.
- `RetroDeathBig_assembler` — assembler variant.

All use `color=40  intensity=0.5  thickness=0.5  duration=0.02`. `color=40`
is an HSV-palette-based hue (see `hs2rgb()` in weapondefs_post.lua line 42);
40/255 ≈ hue 0.157 → orangey-yellow. So death is a burst of thin
orange-yellow streaks radiating outward.

---

## 4. Custom Explosion Generators (CEGs)

CEG TDFs live under [gamedata/explosions/](../../upstream/Kernel-Panic/gamedata/explosions/).
Each is a dictionary of named *spawners*, each with a `class=` (engine class)
and a `[properties]` block. Classes used by System faction:

- `CSimpleParticleSystem` — main particle emitter (particleLife, speed,
  size, colorMap, Texture, airdrag, gravity, emitVector, emitRot,
  sizegrowth, sizemod, directional).
- `CBitmapMuzzleFlame` — a fixed quad/sprite that grows (muzzle/shockwave).
- `CExpGenSpawner` — recursively spawns another CEG after a delay (used by
  `system_nx` and `system_sigterm` to spread ongoing fire over many ticks).
- `groundflash` — a built-in ground decal/ring flash.

`colorMap` format is: a flat list of `r g b a r g b a …` keys describing a
gradient interpolated over the particle lifetime.
`pos=X r Y` means X + random in [0,Y). `… i N` means "repeat every N frames".
`particleSize=12 d-.5` means `12 + (-0.5 * delta)`-style scripted value.

### 4.1 oldskool_shot1 (muzzle puff — soft blue)
[gamedata/explosions/oldskool_shot1.tdf](../../upstream/Kernel-Panic/gamedata/explosions/oldskool_shot1.tdf)
```
[circle]
  Texture=circle (whitecircle.tga, 64x64)
  colorMap=1 1 1 .1   .2 .2 1 .1   0 0 0 0     # white → blue → fade
  sizegrowth=0.1  particleSize=8±2  particleLife=12±2  numParticles=1
```
Used as: pointer `[SFXTypes] 0`, bit `[SFXTypes] 0`, signal `[SFXTypes] 0`,
assembler `[SFXTypes] 0`. Also as Bit's `Line` weapon's per-frame
`explosiongenerator` along the flight path — producing the blue trail the
user perceives.

### 4.2 oldskool_shot2 (muzzle flare — yellow/white)
```
[circle]
  Texture=arrowflare (128x128)
  colorMap=1 1 0.1 .1   1 1 1 .5   0 0 0 0    # yellow → white → fade
  sizegrowth=0.1  particleSize=8±2  particleLife=12±2  numParticles=1
```
Used as: bit `[SFXTypes] 1` (fired by `FireWeapon1`), signal `[SFXTypes] 1`.

### 4.3 oldskool (byte MegaBeam impact)
[gamedata/explosions/oldskool.tdf](../../upstream/Kernel-Panic/gamedata/explosions/oldskool.tdf)
```
[squarecloud]
  Texture=square (solidwhite.tga)
  colorMap=1 1 0 .3  1 0 0 .2  0 0 0 .8  0 0 0 0     # yellow → red → black
  sizemod=.96  gravity=0.1,0,0  airdrag=0.8
  particleLife=12±24  numParticles=15  particleSpeed=2±20  particleSize=14±10
  emitRotSpread=80  directional=0
[tracers]
  Texture=hline (horizontalline.tga)  colorMap=1 1 1 1  1 1 1 1  0 0 0 0
  numParticles=15  particleSpeed=30±20  particleSize=10  particleLife=5±2
  airdrag=.7  directional=1
[groundflash]  flashSize=16  color=1,0.6,0.6
```
This is a *small-but-punchy* impact: smoke/fire cloud plus radial white
tracer streaks. With AoE=128 from MegaBeam and 4 bolts per volley, these
overlap into the big yellow impact flash visible in
[`byte_impact_oldskool.jpg`](frames/byte_impact_oldskool.jpg) —
yellow/orange/red blocky square particles (the `square` texture is
`solidwhite.tga` so the colormap entirely drives color), with stark white
radial `hline` tracers shooting out in the plane.

### 4.4 oldskool_impact (pointer Geometric impact)
[gamedata/explosions/oldskool_impact.tdf](../../upstream/Kernel-Panic/gamedata/explosions/oldskool_impact.tdf)

Three spawners:
- `[circle]` 30 small `circle` particles, yellow→red→black colormap,
  `emitRot=165±15`, `particleSpeed=8±24` — a splatter of red sparks.
- `[bigcircle]` **1** massive white circle, `particleSize=128±5`, life only
  2±2 frames, `sizemod=.9` — a single big white flash ring at ground impact.
- `[tracers]` 15 `hline` tracers shooting radially (`emitRot=80±3`).
- `[groundflash]` flashSize=256 (big).

This is the **ground-splash flash** the user described as "substantial
explosion at the target point" for the pointer shots.

### 4.5 oldskool_big / oldskool_vbig (death CEGs)
Both:
- `squarecloud` (36 particles, yellow→red→black, life 36±48).
- `tracers` (15 white hline tracers, speed 30±20).
- `groundflash` — flashSize=48 (big) or 96 (vbig), ttl 16 / 64.

### 4.6 oldskool_death (regular unit death)
`circle` (growing red/yellow), `squarecloud` (32 persistent white sparks,
zero speed, life 48±16), and 15 `vline` tracers shooting up.

### 4.7 oldskool_build (nano particle)
Single `squarehollow` particle, white→fade, numParticles=1. Used by
BuildLaser's build nano-drip effect.

### 4.8 system_nx (pointer NX Flag — lingering fire field)
[gamedata/explosions/system_nx.tdf](../../upstream/Kernel-Panic/gamedata/explosions/system_nx.tdf)
```
[fire] (CExpGenSpawner, count=240, delay=8±8 frames)
  → custom:system_nx_fire   # respawns itself 240 times
[explosion] CSimpleParticleSystem
  Texture=square  particleSize=12±8  particleLife=90±70  numParticles=30
  colorMap=1 1 0 .3  1 0 0 .2  .4 0 0 .8  .2 0 0 .8  0 0 0 .8  0 0 0 0
  gravity=0,0.05,0  sizegrowth=.2  particleSpeed=3±10
[shockwave] CBitmapMuzzleFlame
  frontTexture=shockwave  size=1 sizeGrowth=120  ttl=6  colorMap=.75 .5 .3 .1 …

[system_nx_fire]
  squarecloud (count=2, particleSpeed=3±5, size=7±4, life=50±20)
  shockwave (frontTexture=circle, sizeGrowth=120, ttl=24)
```
So NX = instant smoke explosion + expanding shockwave, *plus* 240 delayed
respawns over ~240*8 = ~64 s of fire particles scattered in a ±30-elmo
square around impact. Burns forever. Area ~240 elmo radius.

### 4.9 system_sigterm (terminal airstrike — enormous pillar)
[gamedata/explosions/system_sigterm.tdf](../../upstream/Kernel-Panic/gamedata/explosions/system_sigterm.tdf)
```
[risingfire] CExpGenSpawner count=20 delay=2±3 pos=0, 0±23, 0
  → system_sigterm_fire
[downblast] CSimpleParticleSystem emitVector=0,-1,0  pos=0,30,0
  30 red/yellow squares, size 12±8, speed 10±40, life 90±70, emitRot=80±30
[shockwave] CBitmapMuzzleFlame size=3 sizeGrowth=140 ttl=6 pos=0,60,0

[system_sigterm_fire]
  [squarecloud]  12 red squares rising (speed 12, life 50±20, grow 1, gravity -0.1±.01)
  [pillar]       big stationary pillar of dark red stationary squares
                  (size 30±20, life 110d-3 ±48, sizegrowth=0.05, count=4)
```
This builds a vertical red-black pillar with rising-then-falling embers,
a downward shockblast and an initial shockwave ring. Matches a big
mushroom-style cloud.

### 4.10 mine, minelauncher, minekiller CEGs
- `mine` — 48 hollow-square particles radiating, life 25±25; groundflash 64.
- `minelauncher` — single blue→red hex-star particle (`hexastar.tga`).
- `mineclearer` — (used by Minekiller) not inspected but referenced.

---

## 5. COB/BOS piece & animation conventions used by System units

The scripts are compiled to COB with a linear constant (65536 for most,
163840 for byte). Lengths/positions use fixed-point brackets:
- `[N]` = linear elmos (scale 65536 or 163840 per unit depending on header).
- `<N>` = angular degrees × 65536/360 = TAANG units.
- `turn X to AXIS A speed S` interpolates; `now` snaps instantly.
- `spin X around AXIS speed S accelerate A` for rotors.
- `emit-sfx N from PIECE` — 1024+i triggers `explosiongenerator{i}` from the
  FBI `[SFXTypes]` block; 2048+i triggers the same CEG flagged as a projectile;
  smaller numbers fire built-in engine effects.
- `start-script F()`/`call-script` for async animation coroutines, gated
  through `signal SIG_X` + `set-signal-mask SIG_X` so new aim commands
  cancel previous ones.

System-faction-specific animation beats:
- **pointer**: retracts gun when moving (`spin body around x-axis`), pops
  gun back up when stopped; locked `TURNRATE=270` manual heading stepper.
- **bit**: whole `body` piece spin-rolls on x-axis during movement.
- **byte**: `Open()` lifts `base` by 24 elmos, turns `rotor` by 45°, pops 4
  blades outward; `Close()` 3 s timeout reverses it. Firing cycles the 4
  aperture pieces bp0…bp3. `LaunchMines` requires HP > 6000 and an open
  aimer, consumes 6000 HP and lobs mines from `launcher1…5` pieces.
- **BUILD_PERCENT_LEFT rise**: every unit's `Create()` has
  `while(get BUILD_PERCENT_LEFT){ move base to y-axis [-32]*(pct)/100 now; sleep 60; }`
  to rise from sunk-into-ground during construction (16/24/32 elmo sink
  depending on unit footprint). This is the same mechanic our
  factory Rise emerger should fake for System.

---

## 6. 3D models (.s3o) used by weapons/projectiles

All in [upstream/Kernel-Panic/objects3d/](../../upstream/Kernel-Panic/objects3d/)
(copied into this reference dir):

| File             | Used by                             | Texture             |
| ---------------- | ----------------------------------- | ------------------- |
| `cube.s3o`       | pointer unit body                   | `cube.tga` (256c)   |
| `ball.s3o`       | bit unit body                       | (shares cube?)      |
| `octaeder.s3o`   | byte unit body                      | `octaeder.tga`      |
| `octashot.s3o`   | `Geometric` + `nx` projectiles      | `octashot.tga` 8×8  |
| `sigterm.s3o`    | `SigTerm` airstrike bomb            | `sigterm.tga` (?)   |
| `signal.s3o`     | `signal` bomber unit                | `signal.tga`        |
| `logic_bomb.s3o` | `logic_bomb` mine                   | (embedded)          |
| `nullobject.s3o` | `mineblaster`, various invisibles   | —                   |

Note `octashot.tga` is **only 8×8 pixels** — effectively a flat-color texture
for the octahedron projectile. The apparent geometry (small tumbling solid)
comes entirely from the s3o model.

---

## 7. Weapon-ribbon textures (.tga) used by System faction

Copied into this dir from [upstream/Kernel-Panic/bitmaps/kpsfx/](../../upstream/Kernel-Panic/bitmaps/kpsfx/):

| File                     | Size     | Used by                                             |
| ------------------------ | -------- | --------------------------------------------------- |
| `arrow.tga`              | 256×64   | bit `Line` beam main ribbon (`texture1=arrow`)      |
| `arrowflare.tga`         | 128×128  | `oldskool_shot2` (bit muzzle flare)                 |
| `bytemegabeam.tga`       | 32×32    | byte `MegaBeam` end-cap (`texture2=bytelaser`)      |
| `bytemegabeammid.tga`    | 32×32    | byte `MegaBeam` main  (`texture1=bytelasermid`)     |
| `pointershottrail.tga`   | 16×16    | pointer `Geometric` smoke trail + generic `smoketrail` |
| `pointerstarttrail.tga`  | 16×16    | engine `firsttrailtex` (start of smoketrail)        |
| `firetrail.tga`          | 256×64   | pointer `nx` smoke trail                            |
| `whitecircle.tga`        | 64×64    | `oldskool_shot1` and most impact circle particles   |
| `solidwhite.tga`         | n/a      | `square` — generic particle square                  |
| `hollowsquare.tga`       | 64×64    | `oldskool_build`, `mine` ring particles             |
| `horizontalline.tga`     | 64×64    | `hline` — radial tracers in impact/death CEGs       |
| `verticalline.tga`       | 64×64    | `vline` — vertical tracer in `oldskool_death`       |
| `shockwave.tga`          | 256×256  | `system_nx` + `system_sigterm` expanding front      |
| `hexastar.tga`           | 128×128  | `minelauncher` CEG trail particle                   |
| `black.tga`              | 8×8      | `black` — projectile core fill for beams/mines      |

Engine alias bindings are declared in
[gamedata/RESOURCES.TDF](../../upstream/Kernel-Panic/gamedata/RESOURCES.TDF#L49-L103)
under `[projectiletextures]`.

---

## 8. Sound palette (System units)

All from [upstream/Kernel-Panic/sounds/](../../upstream/Kernel-Panic/sounds/):

| Sound          | Unit          |
| -------------- | ------------- |
| `bitfire.wav`  | bit `Line` start |
| `bitdeath.wav` | bit `RetroDeath` + `Minekiller` |
| `bytefire.wav` / `bytehit.wav` / `bytedeath.wav` | byte |
| `pointerfire.wav` / `pointerhit.wav` / `pointerdeath.wav` | pointer |
| `SIGTERMhit.wav` | terminal airstrike |
| `assemblerdeath.wav` | assembler |

---

## 9. Implications for our Rust/Bevy port

Where our `kernel-panic/src/units/combat/` and `weapon_fx/` are today, the
mapping is straightforward:

1. **Bit / Byte beams.** These are NOT instant hitscan. Port as a
   *traveling ribbon* projectile: spawn from QueryWeapon piece, travel at
   `weaponvelocity` toward a predicted point, with a visual bolt length =
   `duration*weaponvelocity`. Render two overlapping quads (outer color +
   inner core) stretched along travel direction, texture from
   `arrow.tga` (bit) / `bytemegabeammid.tga` + end-caps from
   `bytemegabeam.tga` (byte). Tint by `RGBcolor`; inner core white.

2. **Pointer Geometric.** Spawn a 3D tumbling `octashot.s3o` mesh projectile
   with homing (tracks=1, turnrate≈20000/second), arcing trajectory
   (trajectoryheight=1 → arc apex ≈ dist * 1.0), emitting a smoke trail
   sampled from `pointershottrail.tga`. Use `oldskool_impact` particle
   bundle for the ground burst.

3. **Byte MegaBeam burst.** 4 quick shots per volley, each from a different
   turret aperture (bp0..bp3), spread over 1 s at burstrate=0.25,
   sprayangle ≈ 22°. Big AoE 128 impact using `oldskool` particle bundle.

4. **Impact particles.** Translate `CSimpleParticleSystem` CEGs into our
   existing Bevy particle emitters: `colorMap` → gradient over lifetime;
   `sizemod` → per-frame size multiplier; `airdrag` → per-frame velocity
   multiplier; `emitRot±Spread` → cone half-angle; `directional=1` → orient
   sprite along velocity; `particleLife±Spread` → random life.

5. **Ground flash** — port as a shortlived decal / unlit disc with
   `flashSize`-radius, colored per TDF.

6. **Retro death** — when any System unit dies, spawn ~11 short radial
   streaks (`beamweapon` `RetroDeath*`) fanning out at `sprayangle=1536`
   (33.75° cone — effectively 360° because each streak is independent?
   actually `sprayangle` is ±half-cone, so they spray inside a cone but
   since weapon has no target, it fires radially). Plus the associated
   `oldskool_*` ground cloud based on unit size.

7. **Textures & models** — copy the .tga / .s3o files into our assets
   folder; do NOT re-invent. `octashot.s3o` is tiny (970 bytes) and
   self-contained, and the ribbon textures are all small (≤256×256).

8. **Sound effects** — reuse the upstream `.wav`s directly.

---

## 10. Files copied into this ref directory

```
arrow.tga  arrowflare.tga
bytemegabeam.tga  bytemegabeammid.tga
pointershottrail.tga  pointerstarttrail.tga
firetrail.tga
whitecircle.tga  solidwhite.tga  hollowsquare.tga
horizontalline.tga  verticalline.tga
shockwave.tga  hexastar.tga  black.tga
octashot.tga  octa_supplement.tga
octashot.s3o  ball.s3o  cube.s3o  octaeder.s3o
sigterm.s3o  signal.s3o  logic_bomb.s3o
```

No video frames were extracted (ffmpeg not installed); use these
assets plus the `.tdf` files under
[upstream/Kernel-Panic/weapons/](../../upstream/Kernel-Panic/weapons/) and
[upstream/Kernel-Panic/gamedata/explosions/](../../upstream/Kernel-Panic/gamedata/explosions/)
as authoritative reference going forward.
