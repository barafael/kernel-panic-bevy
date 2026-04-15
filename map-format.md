# Spring RTS engine map format: a complete technical reference

The Spring RTS engine — the foundation beneath Kernel Panic and dozens of other games — uses a sophisticated binary map format built around two core files: the **SMF** (Spring Map File) for terrain data and the **SMT** (Spring Map Tiles) for texture tiles. Together with a Lua-based metadata layer and a standard archive container, these components form a system that encodes heightmaps, tiled ground textures, resource distribution, terrain classification, vegetation, and feature placement into a compact, GPU-friendly package. This report documents every known technical detail of this system, drawn from the Spring engine source code, wiki documentation, tool source code, and community knowledge.

---

## The SMF binary format stores all terrain data in a single file

The SMF file is the heart of every Spring map. It begins with an **80-byte header** containing the magic string `"spring map file\0"`, followed by dimension fields, height range floats, and file offsets pointing to each data section. Every integer is **little-endian**; the format has remained at **version 1** since inception.

### SMF header struct (from `SMFFormat.h` in the engine source)

```c
struct SMFHeader {
    char  magic[16];       // "spring map file\0"
    int   version;         // Must be 1
    int   mapid;           // Random unique ID (set to rand())
    int   mapx;            // Map width in spring squares (divisible by 128)
    int   mapy;            // Map height in spring squares (divisible by 128)
    int   squareSize;      // Distance between vertices; always 8
    int   texelPerSquare;  // Texels per square; always 8
    int   tilesize;        // Texels per tile; always 32
    float minHeight;       // World height for heightmap value 0x0000
    float maxHeight;       // World height for heightmap value 0xFFFF
    int   heightmapPtr;    // Offset → short int[(mapy+1)*(mapx+1)]
    int   typeMapPtr;      // Offset → unsigned char[mapy/2 * mapx/2]
    int   tilesPtr;        // Offset → MapTileHeader
    int   minimapPtr;      // Offset → 1024×1024 DXT1 + 8 mipmap levels
    int   metalmapPtr;     // Offset → unsigned char[mapx/2 * mapy/2]
    int   featurePtr;      // Offset → MapFeatureHeader
    int   numExtraHeaders; // Count of extra headers following this struct
};
```

| Offset | Size | Type | Field | Notes |
|--------|------|------|-------|-------|
| 0 | 16 | char[16] | magic | `"spring map file\0"` |
| 16 | 4 | int32 | version | Always 1 |
| 20 | 4 | int32 | mapid | Random GUID |
| 24 | 4 | int32 | mapx | Must be divisible by 128 |
| 28 | 4 | int32 | mapy | Must be divisible by 128 |
| 32 | 4 | int32 | squareSize | Always 8 |
| 36 | 4 | int32 | texelPerSquare | Always 8 |
| 40 | 4 | int32 | tilesize | Always 32 |
| 44 | 4 | float32 | minHeight | Min terrain height |
| 48 | 4 | float32 | maxHeight | Max terrain height |
| 52 | 4 | int32 | heightmapPtr | File offset to heightmap |
| 56 | 4 | int32 | typeMapPtr | File offset to typemap |
| 60 | 4 | int32 | tilesPtr | File offset to tile section |
| 64 | 4 | int32 | minimapPtr | File offset to minimap |
| 68 | 4 | int32 | metalmapPtr | File offset to metalmap |
| 72 | 4 | int32 | featurePtr | File offset to features |
| 76 | 4 | int32 | numExtraHeaders | Extra header count |

The pointer fields mean sections can appear in any order within the file — the engine seeks to each offset independently. A map described as "16×16" in the game lobby has `mapx = mapy = 2048` (16 × 128). The world size in **elmos** (Spring's fundamental distance unit) equals `mapx × 8` along each axis, so a 16×16 map spans **16,384 × 16,384 elmos**.

Immediately after the 80-byte header, `numExtraHeaders` extra header blocks follow. The only well-known type is **type 1 (grass/vegetation map)**:

```c
struct ExtraHeader_Grass {
    int size;        // Size of extra data (4 bytes)
    int type;        // 1 = grass map
    int grassOffset; // File offset → unsigned char[mapx/4 * mapy/4]
};
```

The grass data is an array of bytes at `grassOffset`, where each byte represents grass density (0 = none, 255 = maximum) at a resolution of **(mapx/4) × (mapy/4)**.

---

## Heightmap encoding uses 16-bit signed shorts with linear interpolation

The heightmap lives at the file offset `heightmapPtr` and consists of **signed 16-bit integers** (`int16_t`) arranged in a grid of **(mapx + 1) × (mapy + 1)** samples. The +1 accounts for fence-post vertices at grid boundaries. For a 16×16 map, this means a **2049 × 2049** heightmap consuming roughly 8.4 MB.

Each raw value maps linearly to world height:

```
world_height = minHeight + (raw_value / 65535.0) × (maxHeight − minHeight)
```

Where `minHeight` and `maxHeight` come from the SMF header (or can be overridden in `mapinfo.lua`). Adjacent heightmap vertices are spaced **8 elmos** apart (`squareSize = 8`). Between vertices, the engine interpolates height to produce smooth terrain geometry. The critical sizing rule: **heightmap resolution = (texture_size / 8) + 1** per axis.

---

## SMT tiles use DXT1 compression with mipmaps at 680 bytes per tile

The ground texture is not stored as a monolithic image. Instead, it is split into **32×32 pixel tiles**, each DXT1-compressed with mipmaps, and stored in one or more SMT files. The SMF file contains a tilemap that indexes into these tiles.

### SMT file header (32 bytes)

```c
struct TileFileHeader {
    char magic[16];       // "spring tilefile\0"
    int  version;         // Must be 1
    int  numTiles;        // Number of tiles in this file
    int  tileSize;        // Must be 32
    int  compressionType; // Must be 1 (DXT1)
};
```

After the header, tiles follow sequentially. Each tile is exactly **680 bytes**, comprising the base 32×32 DXT1 level plus three mipmap sub-levels:

| Mip level | Dimensions | DXT1 bytes |
|-----------|------------|------------|
| 0 (base) | 32×32 | 512 |
| 1 | 16×16 | 128 |
| 2 | 8×8 | 32 |
| 3 | 4×4 | 8 |
| **Total** | | **680** |

DXT1 (also called BC1/S3TC) compresses each 4×4 pixel block into 8 bytes using two RGB565 endpoint colors and a 32-bit index table, achieving a **6:1 compression ratio**. The total SMT file size is `32 + (numTiles × 680)` bytes.

### How the SMF references tiles

At offset `tilesPtr` in the SMF, a `MapTileHeader` appears:

```c
struct MapTileHeader {
    int numTileFiles;  // Number of SMT files
    int numTiles;      // Total tiles across all files
};
```

This is followed by `numTileFiles` entries, each containing a 4-byte tile count and a null-terminated filename string. After all file entries comes the **tilemap**: an array of `(mapx/4) × (mapy/4)` 32-bit integers. Each integer is a global tile index — tiles are numbered sequentially across all referenced SMT files. If file A contributes 500 tiles and file B contributes 300, indices 0–499 map to file A and 500–799 to file B.

Each tile covers a **4×4 map-square region** (32×32 texels). The `mapinfo.lua` file can override SMT filenames via `smf.smtFileName0`, `smf.smtFileName1`, etc. During compilation, **tile deduplication** compares tiles for similarity — identical or near-identical 32×32 patches share the same index, dramatically reducing file size. The MapConv `-c` flag controls the deduplication threshold (0 = exact match only, higher values = more aggressive merging).

---

## Metalmap, typemap, and minimap complete the data layers

**Metalmap** (at `metalmapPtr`): An array of unsigned bytes at resolution **(mapx/2) × (mapy/2)** — half the heightmap resolution minus one in each dimension. Each byte encodes metal density from **0 (none) to 255 (maximum)**. Metal extractors placed on the map read these values to determine yield, scaled by the `maxMetal` parameter in `mapinfo.lua`.

**Typemap** (at `typeMapPtr`): Same resolution as the metalmap — **(mapx/2) × (mapy/2)** unsigned bytes. Each byte is a **terrain type index (0–255)** that maps to entries in the `terrainTypes` array defined in `mapinfo.lua`. Terrain types control movement speed multipliers for different unit classes (tank, kbot, hover, ship), surface hardness, and whether vehicle tracks render.

**Minimap** (at `minimapPtr`): A fixed **1024×1024 DXT1** image with 8 mipmap sub-levels (512² through 4²), consuming exactly **699,048 bytes**. This is always embedded in the SMF regardless of map size. MapConv generates it by downscaling the diffuse texture; `mapinfo.lua` can override it via `smf.minimapTex`.

### Dimension quick reference for all layers

For a map of W × H Spring Map Units (lobby size):

| Layer | Resolution | Formula |
|-------|-----------|---------|
| mapx, mapy | — | W×128, H×128 |
| Heightmap | (W×128+1)² | (mapx+1) × (mapy+1), 16-bit |
| Ground texture | (W×1024)² | Via 32×32 tiles in SMT |
| Metalmap | (W×64)² | mapx/2 × mapy/2, 8-bit |
| Typemap | (W×64)² | mapx/2 × mapy/2, 8-bit |
| Tilemap | (W×32)² | mapx/4 × mapy/4, 32-bit indices |
| Grass map | (W×32)² | mapx/4 × mapy/4, 8-bit |
| Minimap | 1024×1024 | Fixed, DXT1 |

---

## Features encode trees, rocks, geovents, and wrecks

At `featurePtr`, the SMF stores placed map objects. The section begins with a `MapFeatureHeader`:

```c
struct MapFeatureHeader {
    int numFeatureType;  // Count of distinct feature type names
    int numFeatures;     // Total feature instances
};
```

This is followed by `numFeatureType` null-terminated strings naming each feature type (e.g., `"TreeType0\0"`, `"GeoVent\0"`, `"Wreckage_Arm_Solar\0"`). Then `numFeatures` instances of:

```c
struct MapFeatureStruct {
    int   featureType;   // Index into the type name list
    float xpos;          // World X position
    float ypos;          // World Y (height) position
    float zpos;          // World Z position
    float rotation;      // Encoded rotation
    float relativeSize;  // Scale factor (typically 1.0)
};
```

Each struct is **24 bytes**. Rotation is encoded as `degrees = −32767 + (rotation / 65535) × 360`.

**Modern maps** increasingly bypass the SMF feature section entirely, instead using a **Lua-based feature placer**. A file at `mapconfig/featureplacer/set.lua` defines feature positions:

```lua
local features = {
    objectlist = {
        { name = 'btreeclo_4', x = 7760, z = 112, rot = "0" },
        { name = 'geovent',    x = 177,  z = 192, rot = "0" },
    },
}
return features
```

A companion gadget (`FP_featureplacer.lua` in `LuaGaia/Gadgets/`) reads this file and spawns features at game start. This approach is more flexible since it doesn't require recompiling the SMF to move a tree.

---

## mapinfo.lua replaced the older .smd format with full Lua scripting

The **mapinfo.lua** file sits at the root of the map archive and returns a Lua table containing all map configuration. It replaced the legacy **.smd** (Spring Map Definition) format, which used a simple INI-like TDF syntax. The engine's `maphelper.sdz` provides backwards compatibility by parsing `.smd` files into the Lua table format.

Key `mapinfo.lua` sections and their most important fields:

**Top-level**: `name`, `description`, `author`, `version`, `mapfile` (path to the .smf), `maphardness` (deformation resistance, default 100), `notDeformable` (bool), `gravity` (default 130 units/sec²), `tidalStrength`, `maxMetal` (default 0.02), `extractorRadius` (default 500), `voidWater`, `voidGround`.

**`smf`**: `minHeight`, `maxHeight` (override SMF header values), `minimapTex`, `metalmapTex`, `typemapTex`, `grassmapTex` (texture overrides added in engine 99.0), `smtFileName0`...`smtFileNameN` (SMT file overrides).

**`atmosphere`**: `minWind`, `maxWind`, `fogStart`, `fogEnd`, `fogColor`, `sunColor`, `skyColor`, `skyBox` (.dds cubemap), `cloudDensity`, `cloudColor`.

**`water`**: `damage` (HP per frame to submerged units), `surfaceColor`, `surfaceAlpha`, `absorb` (RGB depth absorption), `baseColor`, `minColor`, `fresnelMin/Max/Power`, `perlinStartFreq`, `perlinAmplitude`, `shoreWaves`, `forceRendering`, and extensive specular/reflection parameters.

**`lighting`**: `sunDir`, `groundAmbientColor`, `groundDiffuseColor`, `groundSpecularColor`, `groundShadowDensity`, `unitAmbientColor`, `unitDiffuseColor`, `unitShadowDensity`, `specularExponent`.

**`teams`**: Start position definitions as `{startPos = {x = N, z = M}}` indexed by team number.

**`terrainTypes`**: Array indexed 0–255, each entry defining `name`, `hardness`, `receiveTracks`, and `moveSpeeds` table with `tank`, `kbot`, `hover`, `ship` multipliers.

**`resources`**: Paths to `detailTex`, `specularTex`, `splatDetailTex` (4-channel detail), `splatDistrTex` (RGBA distribution map), `normalMap`, `lightEmissionTex`, `parallaxHeightTex`, `grassBladeTex`, and `splatDetailNormalTex` (array of 4 normal textures for DNTS rendering).

**`custom`**: Arbitrary data accessible by Lua gadgets — commonly used for fog, precipitation, and game-specific parameters.

A separate **`mapoptions.lua`** file at the map root defines lobby-adjustable parameters that players can tweak before a game starts.

---

## Map archives use standard compression with a defined directory layout

Spring maps are distributed as **.sd7** (7zip, non-solid), **.sdz** (zip), or **.sdd** (uncompressed directory for development). These are standard archive formats with renamed extensions, read by Spring's Virtual File System.

The internal layout of a modern map archive:

```
MyMap.sd7/
├── mapinfo.lua                    # Primary configuration
├── mapoptions.lua                 # Lobby options
├── maps/
│   ├── MyMap.smf                  # Binary terrain data
│   ├── MyMap.smt                  # Tile textures
│   ├── specular.png               # Optional SSMF textures
│   ├── splatdist.png              # Splat distribution
│   └── details.png                # Detail overlay
├── mapconfig/
│   └── featureplacer/
│       └── set.lua                # Lua feature positions
├── LuaGaia/
│   └── Gadgets/
│       └── FP_featureplacer.lua   # Feature spawner gadget
├── features/                      # Custom feature definitions
├── objects3d/                     # 3D models for features
└── unittextures/                  # Model textures
```

Legacy maps use a simpler layout with just `maps/MapName.smf`, `maps/MapName.smt`, and `maps/MapName.smd`. Spring scans its `maps/` data directory for archives, mounts them via VFS, and looks for `mapinfo.lua` at the archive root. If absent, the engine falls back to parsing the `.smd` file through the `maphelper` compatibility layer.

---

## Map creation tools span command-line compilers to in-game editors

**MapConv** is the original command-line compiler that converts input images (texture BMP/PNG, heightmap, metalmap, featuremap) into SMF + SMT binary files. Beherith's fork (v2.4) added CUDA compression support and feature placement files. Key flags: `-i` (invert heightmap — almost always required), `-c` (tile deduplication threshold), `-x`/`-n` (max/min height), `-t` (texture), `-a` (heightmap), `-m` (metalmap), `-f` (featuremap), `-z` (external DXT compressor path).

**SpringMapConvNG** is a cross-platform C++ alternative by tizbac, using ImageMagick and DevIL libraries. It accepts the same inputs but with slightly different flags (`-th` for compression threshold, `-ct` for compression type, `-features` for a text-based feature list).

**smf_tools** (enetheru) provides modular utilities: `smt_convert` creates SMT files from images (outputting a CSV tilemap), and `smf_cc` assembles the final SMF using the tilemap and SMT as inputs.

**PyMapConv** (Beherith's `springrts_smf_compiler`) offers a modern Python GUI/CLI with both compilation and decompilation capabilities. The latest release (v0.6.3, March 2024) is self-contained on Windows.

**SpringMapEdit** is an older Java-based 3D editor (2008–2009) with heightmap sculpting, texture painting, metalmap/typemap editing, and feature placement. **SpringBoard** is a newer in-game editor used for DNTS painting (diffuse/normal/texture/specular) and feature placement after initial compilation.

### Workflow from heightmap PNG to playable map

The essential steps: (1) decide map size and calculate dimensions — texture must be a multiple of 1024 pixels per side, heightmap = texture/8 + 1; (2) prepare input images at correct resolutions; (3) create `mapinfo.lua` with height ranges, start positions, and terrain settings; (4) compile with MapConv or equivalent, producing .smf and .smt files; (5) place compiled files in a `.sdd` directory with the correct layout; (6) optionally refine in SpringBoard; (7) archive as `.sd7` for distribution.

---

## Technical constraints govern map dimensions and performance

Map sizes must be **even numbers** of Spring Map Units (each SMU = 512 elmos). The `mapx` and `mapy` header fields must be **divisible by 128**. Texture dimensions must be **multiples of 1024 pixels**. Typical competitive maps are 8×8 (4096×4096 texture, 513×513 heightmap) or 16×16 (8192×8192 texture, 2049×2049 heightmap). Maps can be rectangular (e.g., 6×10).

The **practical maximum** is **32×32** (16,384×16,384 texture, 2049×2049 heightmap). Maps beyond this size face GPU memory exhaustion, visual clipping at distance, and performance degradation. The theoretical int32 tile index limit allows billions of tiles, but a 32×32 map already requires over 1 million tilemap entries. The now-deprecated SM3 format required square, power-of-two dimensions exclusively.

**Key unit conversions**: 1 elmo = the fundamental distance unit. 1 map square = 8×8 elmos. 1 tile = 4×4 map squares = 32×32 texels. 1 Spring Map Unit = 128 map squares = 512 elmos. The coordinate system has X increasing rightward, Z increasing downward (from top-left origin), and Y as height.

---

## Kernel Panic maps replace metal spots with datavents for factory placement

Kernel Panic is a free, open-source "sublimated RTS" running on Spring where Systems, Hackers, and Networks battle in a digital matrix. It has **no resource economy** — all units are free. This fundamentally changes how maps work.

In standard Spring games, maps define metal spots where resource extractors are built. Kernel Panic ignores metal entirely and instead uses **geothermal vent positions (geovents)** — called **"datavents"** in KP's computer-themed lore — as the **only locations where factories can be placed**. The engine's `geoThermal=true` feature property enables this restriction. KP's game logic forces all factory buildings (Sockets for System, Ports for Network) to snap exclusively to datavent positions. This means the map designer directly controls the strategic landscape: datavents near starting positions serve as early expansion sites, while contested central datavents become key objectives.

A Lua widget renders all datavent positions on screen so players can identify build locations. A fallback gadget (`game_spawn.lua`) attempts to place geovents at metal spot positions on maps that lack explicit geovents, though this requires the map to still define the geovent feature type.

### The wireframe aesthetic shapes every visual choice

KP's distinctive look is **"neon bright colors on pure black"** — vectorial, not bitmap. Maps use **dark textures** (black or near-black) as terrain, overlaid with circuit board patterns, grid lines, and digital motifs. Units rendered as bright wireframe neon shapes (green, red, blue, cyan) pop against this dark backdrop, evoking Tron and Darwinia. The maps serve as a "motherboard" stage for the digital warfare.

### Maps bundled with Kernel Panic

The game (version 4.9, targeting Spring 105, released June 2021) includes several maps with computer-themed names:

- **Marble Madness** (by Boirunner, Public Domain) — the classic KP map with a central hill as key strategic terrain
- **Direct Memory Access** (Public Domain) — named after the DMA hardware concept
- **Central Hub**, **Corrupted Core**, **Dual Core**, **Quad Core** (by TradeMark, CC BY-SA) — processor and network themed
- **Major Madness**, **Speed Balls 16 Way** — multiplayer maps (license status uncertain)
- **Hex Farm 7/8** (by zwzsg) — a technically complex map featuring hexagonal grids and dynamic heightmap modification during gameplay

Maps designed for KP should include geovents for factory placement, use fixed start positions, maintain the dark neon digital aesthetic, and mention "kernel panic" in their description for lobby searchability. The game's internal Lua menu curates a whitelist of compatible maps.

---

## Conclusion

The Spring map format is a well-engineered system where a single SMF binary file acts as an index into distinct data sections — heightmap, tilemap, metalmap, typemap, minimap, and features — each at its own resolution and encoding. The tile-based texture system via SMT files achieves strong compression through DXT1 encoding and deduplication while maintaining GPU-friendly data alignment. The evolution from rigid `.smd` configuration to full Lua scripting in `mapinfo.lua` gave map authors programmatic control over every environmental parameter, from Fresnel water reflections to per-terrain-type movement modifiers.

For Kernel Panic specifically, the most critical map design element is **datavent placement** — it transforms standard Spring terrain into a strategic board where expansion is gated by control of fixed geographic points, perfectly complementing the game's zero-economy, fast-paced design philosophy. The format's flexibility in supporting both resource-based economies and KP's geovent-gated factories through the same feature placement system demonstrates the engine's extensibility.

The format's constraints — tiles fixed at 32×32 DXT1, heightmap at 1/8 texture resolution plus one, map dimensions divisible by 128 — are architectural choices optimized for GPU texture streaming and LOD management. Understanding these constraints is essential for anyone building tools that read, write, or convert Spring maps.