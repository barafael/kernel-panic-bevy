# M4 — the Rosetta trail divergence

Since 2026-10-21 `determinism.yml`'s "compare trails" fails now and then with
`macos-x86_64-rosetta diverges from linux-x86_64-gnu`: the x86_64 macOS build,
run under Rosetta 2 on the arm64 runner, writes a hash trail that parts from
the Linux reference at some tick. Every other platform (arm64 macOS, Windows,
musl, arm64 Linux, the four wasm runtimes) has matched Linux on every run.
This is what was established on 2026-10-26, and what it means.

## 1. What it looks like

| run | log | first differing tick | first differing part | lasts |
|---|---|---|---|---|
| 39673773850 (10-21) | Speed Balls | 6142 | positions | to the end |
| 39777427248 (10-22) | Dual Core | 6157 | projectiles | 11 ticks, then identical |
| 39820192834 (10-22) | Speed Balls | 2034 | positions | 1 tick, then identical |
| 40023267835 (10-23) | Dual Core | 2283 | positions | to the end |
| 40327279322 (10-26) | Quad Core | 2648 | health | to the end |
| probe 40350317806 | Quad Core | 17928 | positions | to the end (73 ticks) |
| probe 40371194065 job 3 | Quad Core | 8590 | positions | 1 tick, then identical |

Orders are never the first part to differ. In the Dual Core 6157 case only one
projectile's position differed for eleven ticks; it hit one tick later than on
Linux, the projectile count was off by one for a tick, and the trail was
identical again from 6169 on.

Zero of about eighteen main runs before 10-21 diverged; five of about ten
since. The runner image (`macos-26-arm64` 20261013.0424.1, macOS 26.6.2) and
the toolchain (rustc 1.99.1 since 10-15) are the same on both sides of the
switch, so a sim change in f0b35fd..a0b3c20 (the perf-2026-10 merge) made it
reachable. The diffs there are refactors with no numeric change in sight.

## 2. What it is not

- **Not per host, not per build.** Probe run 40350317806 replayed every log
  four times in a row on one runner with one binary; the second Quad Core
  replay diverged, the other fifteen matched. Runs of the very same sim code
  on main went both ways (40325795600 same, 40327279322 diverged, 40337790396
  and 40338273605 same). Every probe runner reported "Apple M1 (Virtual)".
- **Not thread timing the sim is sensitive to, as far as Linux can show.**
  Four local replays of every log — plain, pinned to one core, pinned to one
  core under eight busy loops, niced under load — all byte-identical to CI's.
- **Not a per-thread float environment.** `floatprobe threads 8` under
  Rosetta: every thread's MXCSR is 0x1fb7 and every probe's bits equal the
  main thread's.
- **Not libc.** `floatprobe memcmp` (30 M byte comparisons, equality tests
  and record sorts at random lengths and alignments against scalar loops) and
  `floatprobe memcpy` (4.7 GiB of memcpy, memmove and `Vec` growth on four
  threads, every byte checked against a formula) found nothing under Rosetta.
  Rosetta advertises SSE4.2 and no AVX on these runners.
- **Not the sim's state, in the one case caught with dumps.** `--expect-trail`
  (new: the replay compares every tick's hash with the reference as it goes
  and dumps the first differing tick and the one before) caught Quad Core at
  tick 8590. The Rosetta dumps of ticks 8589 and 8590 are identical to the
  Linux dumps of the same ticks: every unit's position and rotation, every
  feature, every projectile, every mover; only the entity ids differ by the
  usual allocation offset. The positions hash of 8590 is `917a6fc762a3f386`
  on Linux and `3a465af2b80aa3d6` under Rosetta — **the same values, hashed
  to a different number.**

## 3. Forensics on that hash

The positions part hashes 570 byte records (388 units, 182 features), sorted
as byte strings, FNV-1a over each with its length. With the Linux records of
tick 8590 in hand (`hashforensics`, scratch), none of these reproduce the
Rosetta number: the unsorted or reversed order; a signed-byte or length-first
sort; any one record dropped, duplicated, or moved anywhere; any pair swapped;
any single bit or single byte changed; any float or id field changed by up to
±1024 units in the last place; any one record, its translation or its rotation
taken from tick 8589. Whatever Rosetta hashed was further from the Linux
records than any one-step fault.

## 4. Reading (superseded by §3c)

*Written before the records dumps: the hash was right, the read of one float
was wrong. Kept for the record.*

The one divergence seen in full had correct sim state and a wrong hash; the
hash is plain integer code over bytes, and its inputs, the libc it calls and
the float environment all check out. The remaining explanation is Rosetta's
own execution of this binary being wrong once in a long while: at a rate of
about one process in ten over four replays (~80 000 ticks), with no pattern in
which log or tick. The persistent divergences fit the same cause landing in
sim arithmetic instead of the hash (a projectile a hair off, a position a hair
off), which is what their first differing parts look like.

Whether that is a Rosetta bug, a macOS 26 VM artefact or something about this
binary cannot be told from the outside; it is not something the sim does. The
platform the job stands for does not exist in the field: an Intel Mac runs the
x86_64 slice natively, and an Apple-silicon Mac runs the arm64 slice, which
matches Linux on every run.

## 5. What to do with the job

*Updated after §3c.* The job cannot be made to pass reliably: the sim tests
floats for NaN in many places, Rosetta answers one such test in five billion
wrongly, and a Quad Core replay asks more than that. The game is not at fault
and no shipped build runs under Rosetta (an Intel Mac runs the x86_64 slice
natively, Apple silicon the arm64 one). What the job can still assert is that
Apple's x86_64 libm agrees with Linux, which the float probe does on its own.
So: make the Rosetta trail comparison advisory (or drop the replay and keep
the float probe), and report the fault to Apple with `floatprobe parity`,
which is 40 lines of asm and a loop.

Options, in order of preference:

1. Keep the Rosetta replay but make its comparison advisory: report its
   divergence in the summary without failing "compare trails". The signal
   that Apple's x86_64 libm agrees with Linux stays (the float probe, which
   has never differed), and a Rosetta that starts diverging on every run
   would still be noticed.
2. Drop the platform from the matrix.

Either way the probe branch (`rosetta-probe`: six-to-ten single-replay jobs
per push, each with `--expect-trail` dumps, the float, memcmp and memcpy
probes, and the hash's own self-check) stays available for more samples.

## 3b. Run 40482252987: the bytes differ, and it is the movement

The first catch with `.records` dumps (job 3, round 3, Quad Core, tick
3975, positions part, persisting): Linux re-hashes Rosetta's bytes to
Rosetta's value, so the bytes differ and the hash was right. Exactly one
record differs: unit #325, an Assembler of team 6, walking to a build
site. At tick 3974 the two positions are still identical (the hash
agrees), but the mover's hidden state is not: on Rosetta it had just
had `start_moving` called again for the same goal (`last_repath_frame`
3470 = the current frame, `prev_wp_dist` 0, a freshly searched path with
`current: 1`), on Linux it was 10 waypoints into the path it had since
frame 3093. The next tick Rosetta's unit turns hard (`turn_speed`
−239.8, heading 3676 → 3437) and slows to 1.45, Linux's does not, and the
positions part.

`start_moving` fires when the mover's goal differs from its `MoveTarget`
(`ground_move.rs`, orders → StartMoving). The target is hashed every
tick (orders part) and never differed, so Rosetta's mover must have lost
its goal (`stop_engine`, target removed) and regained it inside one
tick, or earlier in bookkeeping the hash never saw. The root tick is
earlier than 3974 and in state that was neither hashed nor dumped. §3's
"wrong hash over identical state" reading was wrong: the unit dump that
looked identical left out exactly this.

So the trail now carries a tenth column, the hidden-state hash
(`Capture::hidden_hash`): every mover's `GroundMover` and `MovePath`
and `BuildSiteWait` as `Debug` text, and the path service's state
(`PathQueue::describe`), keyed by `NetId`. Identical across processes
and under the storage shuffle. Main's trail has no such column, so the
probe workflow makes its own Linux reference first. The next catch
names the first tick and the field.

## 3c. Runs 40516621326 and 40542884297: one float, one flag

With the hidden-state column and a repeat capture per tick, the fault shows
itself inside one process, no Linux needed. Over 128 processes: nine ticks
where a repeat capture of the same world, in the same exclusive system with
nothing running in between, produced other bytes than the capture before or
after it. Every one of them is a single `f32` word read as `0x7fc00000`, the
canonical NaN, by one capture and as its value by the others, with that
capture's NaN counter at zero:

| run / job | log, tick | part | what | value |
|---|---|---|---|---|
| 40516621326 / 2 | Quad Core 8540 | hidden | (not dumped) | |
| 40516621326 / 2 | Quad Core 13005 | positions | (not dumped) | |
| 40516621326 / 3 | Quad Core 13041 | hidden | #1025 waypoint 11 x | 512.0 |
| 40516621326 / 4 | Dual Core 5579 | hidden | #472 waypoint 11 x | 2368.0 |
| 40542884297 / 4 | Quad Core 16345 | orders | #4554 move target x | 2852.0 |
| 40542884297 / 1 | Dual Core 6350 | hidden | #758 waypoint x | 1464.0 |
| 40542884297 / 1 | Quad Core 9588 | hidden | #2421 waypoint 0 x | 3984.6 |
| 40542884297 / 1 | Quad Core 11470 | health | #803 max hp | 1000.0 |
| 40542884297 / 1 | Dual Core 6005 | hidden | #743 waypoint x | 1856.0 |

Three of them were the first capture, whose hash goes into the trail, and
those are the "divergences" the job reports; the rest healed unseen. The
earlier persistent divergences (§1) are the same read landing in the sim
itself: a NaN, or a value a few instructions stale, in a unit's path or
target, and the unit turning where Linux's did not (§3b).

The standalone `floatprobe waypoints` (the game's byte writer over mover-like
structs, nothing else) reproduced it under Rosetta: one fault in 3.5 million
ticks captured three times (run 40542884297). The x86_64 code explains the
pattern. For a `Vec3`, rustc emits for the x lane one `ucomiss` of the float
with itself, `setp` to keep the parity flag (the NaN bit) for the counter, a
few flag-preserving moves, then `jp` on the same flags to choose the bytes;
the y and z lanes go through a branchless `cmpunordps` blend. Scalar
`Bits::f32` calls (health, max) get the same `ucomiss`/`setp`/`jp` window.
Every fault is an x lane or a scalar; never a y or z. One compare's flags,
read twice a few instructions apart, disagreed: the counter saw "ordered",
the branch saw "unordered". That is not something a program can do to
itself; it is the emulator's flag state across those instructions, most
plausibly lost or recomputed wrongly around a preemption, which is why it
takes load (four processes on three cores), takes hours, and lands anywhere.

`floatprobe parity` is that window alone, in asm, on floats that are never
NaN, with an integer `cmp`/`sete`/`je` twin beside it, three threads, a
billion windows a second each. On real hardware the two readers always agree.
Under Rosetta on the runner: 253 of 1.43 trillion float windows disagreed in 40 minutes (run 40557952863), every one of them `setp` 0 and `jp` 1, the branch seeing unordered where the register read just before did not, and none of the 1.43 trillion integer windows beside them. The waypoints probe in the same job tripped 17 times in 1.36 million ticks. The fault is the parity flag after `ucomiss` under Rosetta 2 on these runners (macOS 26.6.2 on Apple M1 virtual machines); nothing in the game or the hash. It reads as a float NaN check that spuriously says NaN, once in some five billion checks, and so lands in any code that tests a float for NaN and then uses it, which the sim does all over.

## 6. Tools

- `kernel-panic --replay … --expect-trail <dir> --dump-dir <d>`: dumps the
  first tick whose trail line misses `<dir>/<stem>.trail` and the tick before.
  The dumps list projectiles now. Beside each `<stem>_<tick>.txt` comes
  `<stem>_<tick>.records`: the four part hashes' exact input bytes (the
  sorted records, one hex line each, under a `part <i> <hash>` line).
- `kernel-panic --check-records <dump.records>…`: hashes those bytes on
  this machine and compares with the hashes they were dumped with. This is
  the question §3 brute-forced, asked directly: a Rosetta dump that Linux
  re-hashes to Linux's trail value is the same bytes hashed wrongly, and
  those bytes are then a 30 KB input to feed the hash a billion times; one
  that re-hashes to Rosetta's value is a state difference the unit dump
  did not show, and the diff of the two `.records` files names the record.
  The probe script also re-hashes every dump in a fresh Rosetta process.
- `KP_CAPTURE_REPEAT=<n>`: every tick's state captured n more times in the
  same process, bytes and hash compared with the first capture apart.
- `floatprobe threads [n]`, `floatprobe memcmp [iters]`,
  `floatprobe memcpy [iters] [threads]`, `floatprobe stress [secs] [threads]`
  (all clean under Rosetta; §2).
- `floatprobe waypoints [secs] [threads]`: the game's byte writer over
  mover-like structs, three captures a tick compared (§3c; trips under
  Rosetta). `floatprobe parity [secs] [threads]`: one `ucomiss`, `setp`,
  moves, `jp` window in asm, and its integer twin (§3c; the reproducer).
- `.github/workflows/rosetta-probe.yml`, `.github/scripts/rosetta-probe.sh`:
  since 10-27 one process per golden log, all four at once, four rounds a
  runner, four runners: 64 process samples a push of the branch.
