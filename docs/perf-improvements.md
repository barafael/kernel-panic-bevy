# Kernel Panic (Bevy) — Future Performance Improvements

Notes from the performance pass (2026-09). Baseline instrumentation is in
place: FPS/frame-time logged every 5 s (`LogDiagnosticsPlugin`), and
opt-in per-system profiling via `KP_TRACE=1 cargo run -p kernel-panic`
(writes `trace-kp.json`, open in Perfetto / chrome://tracing).

## Already done

- **Dev-profile dependency optimization** (`Cargo.toml`):
  `[profile.dev.package."*"] opt-level = 1, debug = false`. This was the
  single largest win (opt-0 bevy/wgpu/glam dominated everything). opt-2/3
  OOM-kills rustc on ~9 GB free RAM (bevy_pbr), so opt-1 is the practical
  ceiling here.
- **movement_system spatial hash**: collision resolution was O(moving ×
  all-units) per frame (1.3 ms at ~250 units). Now a per-frame uniform
  grid (32-elmo cells, 3×3 neighborhood) feeds `resolve_motion` and
  `waypoint_blocked_by_arrived_unit`.
- **update_hover ray-cast gating**: mesh ray-cast now runs only when the
  cursor or camera moved, plus an 8-frame refresh (was every frame,
  0.33 ms).
- **Building command churn**: factories no longer issue three no-op
  `remove` commands per frame.

## Measured candidates (from the 2026-09 trace, ~250 units, 4400 frames)

Durations are per-frame averages at the time of measurement:

1. **COB animation VM (0.21 ms/frame, grows with unit count)**
   `animation::publish_unit_values` + `animation::animation_system` step
   every unit's COB thread every frame. Options: (a) run at 30 Hz via a
   fixed-timer gate (halves cost, visually indistinguishable), (b)
   carcinize the remaining per-unit animations into data-driven tweens
   (see `docs/kernel-panic-game-design.md` — animation strategy), which
   removes the interpreter from the hot path entirely.

2. **Render-side PBR costs (bevy-internal, ~2.5 ms combined)**
   `collect_meshes_for_gpu_building`, `allocate_and_free_meshes`,
   `prepare_erased_assets<StandardMaterial>`, `prepare_clusters`,
   `queue_material_meshes`, … These scale with entity/material count.
   Levers: reduce shadow-casting lights, MSAA off, bloom resolution
   down, `NotShadowCaster` on small FX sprites, GPU-driven
   preprocessing already on. Profile with the render-trace spans before
   touching anything.

3. **Material churn (0.3 ms `prepare_erased_assets`)** — any per-frame
   `Assets<StandardMaterial>` mutation re-uploads. Cloak-fade materials
   and emerging-unit fade materials mutate per unit; consider a small
   fixed palette of shared translucent materials + per-entity opacity
   via vertex colors instead of unique material handles.

4. **Entity churn in FX** — geovent puffs (~8 spawns/frame at 120 fps),
   weapon FX sprites. If entity counts grow, move to a single
   `bevy_sprite` particle atlas or `bevy_fixed` instancing.

5. **PresentMode::Immediate** — pinned by the Windows resize workaround
   (see `main.rs` comments). Uncapped FPS burns GPU/CPU; restore
   `AutoVsync` once the winit modal-loop fix lands, and re-measure.

6. **Ambient-load measurement noise** — FPS on this machine swings 34–77
   with desktop load; when comparing runs, prefer per-system trace
   totals over wall FPS.

## Deferred

- Minimap texture rewrite frequency (not hot today, re-check after unit
  counts grow past ~1000).
- `ui/menu` map-list minimap previews would need an async off-thread
  `spring_map::load_map` parse per hovered map — cache aggressively.
