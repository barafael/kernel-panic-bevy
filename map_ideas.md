# Map Ideas

CS-pun names with motherboard/board-game feel. Each tries to make the *terrain shape* match the *concept*, since datavent placement is the only real strategic primitive.

## Stack Overflow (Volcano) — *implemented*

Central caldera on a 4-player map; the stack literally overflows. Every 5 minutes the volcano erupts in a ~13 s sequence: bad blocks ring the slope, cloaked logic bombs land further out, a virus swarm boils up to the rim, ICMP packets fan out radially, and one faction-nibble of each *other* faction lands in each enemy base. Intensity scales with eruption count. See [kernel-panic/assets/maps/Stack_Overflow.sdz](kernel-panic/assets/maps/Stack_Overflow.sdz), [spring-map-gen/src/bin/gen_stack_overflow.rs](spring-map-gen/src/bin/gen_stack_overflow.rs), and [kernel-panic/src/map_events/mod.rs](kernel-panic/src/map_events/mod.rs).

## Stack Overflow (Ziggurat)

Terraced ziggurat. Each ascending tier is a "stack frame" with one datavent. Climbing the stack rewards you with map control but the top is a kill-box once it overflows. Heightmap does most of the work.

## Heap Fragmentation

Mostly-flat plain pock-marked with irregular raised "allocated blocks" of varying sizes, separated by void/free-memory channels. Datavents sit on random blocks, never on the floor — turns expansion into a packing problem.

## Race Condition

Strict mirror, two factions, two parallel "threads" (lanes) with a single shared datavent at the join. First side to reach it locks the critical section.

## Deadlock

4-player rotational symmetry. Each player's natural expansion path runs through the next player's flank, so nobody can grow without breaking the cycle.

## Null Pointer

Donut. A central uncrossable void (literal `voidGround`) with all datavents on the surrounding ring. Fighting is always over the rim; nobody dereferences the middle.

## Segfault

Plateau split by deep fissures into 4–6 isolated "segments", connected only by narrow ridges. Each segment has 1–2 datavents; control is binary per segment.

## Cache Miss

Concentric L1/L2/L3 rings. Inner ring datavents are close and cheap; outer ring datavents are far but plentiful. Players choose latency vs. throughput.

## Circular Buffer — *implemented*

Pure ring corridor, no interior. Players spawn at evenly-spaced indices and read/write around the ring. Wrapping around behind an enemy is the entire game.

Implementation: a flat ring corridor between an inner impassable void crater and an outer cliff wall, 4 player starts and 8 datavents on the corridor centerline. Movement is biased clockwise via the [`CircularFlow`](kernel-panic/src/map_events/circular_flow.rs) resource — travel with the flow runs at ~1.6× speed, against it ~0.4× (4× ratio). Producer-consumer chase: you naturally pursue the player ahead and get pursued by the one behind. See [kernel-panic/assets/maps/Circular_Buffer.sdz](kernel-panic/assets/maps/Circular_Buffer.sdz), [spring-map-gen/src/bin/gen_circular_buffer.rs](spring-map-gen/src/bin/gen_circular_buffer.rs).

## Pipeline

Long, narrow rectangular map (e.g. 6×16) divided into 4–5 stages by gentle ridges. Datavents one-per-stage. Forces front-line progression rather than flanking.

## Endianness

Two-player mirror where one half's heightmap is bit-reversed relative to the other — high ground on the left maps to low ground on the right. Reads symmetric on the minimap but plays asymmetric.

## Heat Sink

Radial fins. A central CPU die with finned ridges radiating outward; datavents in the troughs between fins. Kbots love the fins, tanks live in the troughs.

## Fork Bomb

8-player FFA. Tiny starting alcoves around the rim, each with one datavent, all feeding into a chaotic open center with 2–3 contested vents. Designed to die fast and loud.

## Boot Sector

Deliberately tiny (4×4) duel map. Two datavents per side, one in the middle. The KP equivalent of a chess opening trainer.
