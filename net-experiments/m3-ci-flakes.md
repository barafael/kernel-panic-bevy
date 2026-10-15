# M3 — two intermittent CI failures after M2

Two jobs on `main` failed now and then right after M2 landed:
`determinism.yml` "replay (wasm-webkit)" (a panic, then a hang until the 30-minute
timeout) and `webrtc.yml` "browser <-> native" ("the room never filled"). Neither
was caused by M2's code. Both reproduced or explained locally; this is what they
were and what changed.

## 1. WebKit: JavaScriptCore corrupts the replay's memory (concurrent BBQ tier-up)

**Symptom (CI).** WebKit 26.0 (Playwright 1.56.1, `webkit-2215`) played Dual_Core,
Speed_Balls and Marble, then, two seconds into `ffa8_quad_core` (still in the
match setup), panicked in `bevy_ecs-0.20.0/src/schedule/node.rs:649` "System with
key SystemKey(31v1) does not exist in the schedule" (b9ea779: `SystemKey(28v1)`),
followed by "RuntimeError: Unreachable code". The panic is a wasm trap inside a
promise callback, so `kp_replay`'s promise never settled and `page.evaluate`
waited until the job's timeout (and, before 71ab33f, 4 h). 2 of the 4 runs after
M2 failed this way; Chromium, Firefox and Node never did.

**Reproduction.** The CI artifacts of run 38784458804 (`kp-replay-wasm`,
`kpmaps-determinism`) in `mcr.microsoft.com/playwright:v1.56.1-noble` (CI's
Ubuntu 24.04 and Playwright's WebKit build), `kp-replay.mjs webkit … ffa8 … 30`
(the failure is in the first seconds, so 30 ticks suffice), many times:

| build / JSC option | runs | failed | how |
|---|---|---|---|
| CI build (7ee4ff2) | 66 | 5 | 3 page crashes, 1 out-of-bounds trap, 1 hang (main thread spinning) |
| pre-M2 build (1bbad34, its own golden) | 40 | 1 | the same `SystemKey(22v1)` panic |
| local build with names (`wasm-opt -g`) | 60 | 2 | OOB trap, crash |
| `JSC_useOMGJIT=false` | 40 | 5 | the `SystemKey` panic, a dlmalloc assertion, OOB traps |
| `JSC_useWasmOSR=false` | 40 | 8 | panic, OOB traps, 2 hangs |
| `JSC_numberOfWasmCompilerThreads=1` | 40 | 2 | panic, crash |
| **`JSC_useBBQJIT=false`** | 40 | **0** | |
| **`JSC_useConcurrentJIT=false`** | 100 | **0** | |

30-tick runs. Full-length replays fail too, and not only `ffa8_quad_core`: of the
four logs played once each with JSC's defaults, Dual_Core and Marble ended in
out-of-bounds traps (the same call chain as CI's panic, `wasm-function[5146] →
[25108] → [19870] → [6426] → [34455]`).

**Root cause.** Not our code and not M2: the wasm is single-threaded (no shared
memory, no atomics) and deterministic up to timing, the same bytes never fail in
three other engines, the pre-M2 build fails the same way, and the failures are the
signature of corrupted linear memory: a slotmap key that isn't there, dlmalloc's
own consistency assertion, out-of-bounds traps, a spinning main thread, a
segfaulting WebProcess. They need JavaScriptCore's BBQ tier *and* background
compilation: with BBQ off (IPInt → OMG only) or with compilation on the main
thread (`useConcurrentJIT=false`) 140 runs passed; with OMG off, OSR entry off or a
single compiler thread they still fail. So it is a race in JSC's concurrent
IPInt → BBQ tier-up (code installed while the function runs), mostly hit early,
while the setup's many cold functions tier up. Why M2 seemed to start it: it
didn't — the rate is a few percent per page and pre-M2 runs were lucky; M2 made
the module ~590 KB bigger (matchbox, the lobby), which shifts which functions tier
up when.

**Fix.**

- `kp-replay.mjs` launches WebKit with `JSC_useConcurrentJIT=false`
  (`KP_WEBKIT_CONCURRENT_JIT=1` keeps JSC's default, for checking a newer WebKit).
  This still runs every tier JSC has (IPInt, BBQ, OMG), only compiled on the main
  thread, so the cross-engine float check keeps its meaning. `useBBQJIT=false`
  also cures it but is ~14× slower (Dual_Core 479 s instead of 35 s).
- Fail fast: the runner races `page.evaluate` against the page's first error, a
  panic message on the console, or a page crash, and exits 1 at once (the job
  used to wait for its 30-minute timeout). `kp-netpeer.mjs` does the same. Page
  errors print their stack.

**Open.** Real Safari players run the same JSC: a long or setup-heavy match may
crash or desync there. Worth a WebKit bug with a reduced test case (the failure
needs no rendering: the replay build in a headless page reproduces it); until
then, a desync report from a Safari peer should be read with this in mind.

## 2. Browser ↔ native: the handshake waited ~40 s for Google's STUN

**Symptom (CI).** Run 38778234356: the browser gave up after 120 s ("the room
never filled") and the native peer never saw it either. The runs before and after
passed, and both took **39.9 s** from opening the room to meeting (browser
side: "room of 2 in 39.9153s" / "39.9199s"), on a localhost signaling server.

**Why.** matchbox always hands the browser an ICE server — with our empty list
its default, Google STUN (`MatchboxTransport::open`; a browser refuses an
`RTCIceServer` without URLs) — and its web handshake waits for ICE gathering to
complete before it sends the answer (`wait_for_ice_gathering_complete`).
Gathering completes only when every STUN request has an answer or has timed out.
On GitHub's runners Google's STUN never answered, so every handshake sat ~40 s;
locally, with the STUN packets silently dropped (a network namespace whose default
route ends in a dummy interface, Google's names pinned with
`--host-resolver-rules`), Chromium's headless shell (Playwright's, and the system
one) waits 10.1 s every time (10 / 10 runs). A run whose handshake took longer
than the browser's 120 s (a slower STUN/DNS timeout, a retransmit) had no second
chance: matchbox never times out or retries a handshake, so a stalled pair waits
forever. I could not make the 40 s case fail locally (10 runs with STUN dropped
all met after ~10 s), so the exact cause of the one failure is not proven; what
is certain is that the test depended on the internet and spent its time budget
in a STUN timeout.

**Fix.**

- The test runs its own STUN server (`matchbox_tests::Stun`, a binding-response
  responder on `127.0.0.1`) and hands it to the browser (`KP_NETPEER_ICE` →
  `kp_net_peer(signaling, ice, …)` / `kp_net_probe`, `KP_ICE` syntax; empty =
  matchbox's default). Gathering completes at once: **room of 2 in 135–380 ms**
  instead of 10 s (namespace) / 40 s (CI), and no packet leaves the machine.
- `meet_browser`: once everyone is in the room (the signaling server's view) and
  no data channel opened within 10 s × attempt, the native peer leaves the room
  and rejoins under a new id, which starts a fresh handshake (exercised with the
  STUN delay and a 5 s patience: attempt 1 gave up, attempt 2 met and the trails
  agreed). Both browser tests use it.
- `KP_BROWSER_ARGS` also takes a JSON array (flags with spaces).

## 3. Runs (this machine, shared with other builds; Playwright 1.56.1 image)

- **WebKit, new runner** (concurrent JIT off by default): 60 / 60 short `ffa8` pages;
  the four logs at full length twice, every final hash equal to CI's reference
  (`939e3a8b…`, `1dc39f68…`, `5f4ad3e1…`, `8bfcd92c…`), Dual_Core 42–53 s, Speed_Balls
  35–57 s, Marble 10–13 s, Quad_Core 59–76 s (CI with the concurrent JIT, when it
  passes: 35 / 28 / 4.6 / 60 s). With the earlier batches: 160 short pages and 12
  full replays without concurrent JIT, no failure. Chromium through the new runner:
  unchanged.
- **Fail fast:** panics (`SystemKey`, dlmalloc), out-of-bounds traps and page crashes
  all ended the run with status 1 within a second of happening (the crashes
  themselves came 14–105 s in); only a hang without any error still needs the job
  timeout.
- **`browser_peer` with the fix:** 15 / 15 in a namespace with Google's STUN dropped
  (Playwright's headless shell), 5 / 5 on the host network (docker, Playwright image);
  room of 2 in ≤ 0.4 s every time; the trails agreed over 862–899 ticks. Without the
  fix, STUN dropped: 11 / 11 passed, each handshake 10.1 s. `browser_probe`: 1 / 1.
- The rest: `cargo test --workspace` and the four clippy runs clean.
