//! `floatprobe run <out.bin>`: write every probe's result bits.
//! `floatprobe compare <a.bin> <b.bin> [...]`: per-probe differing inputs
//! for each pair; exits 1 if any `det:` probe differs.
//! `floatprobe threads [n]`: every probe on `n` threads at once, each
//! compared with the main thread's bits, with each thread's float
//! control register and a few denormal results (the Rosetta probe:
//! does a thread's float environment differ from the main thread's?).

use std::process::ExitCode;

/// The float control register of the calling thread: MXCSR on x86_64
/// (bit 15 FTZ, bits 13-14 rounding, bit 6 DAZ), FPCR on aarch64 (bit
/// 24 FZ, bits 22-23 rounding, bit 19 FZ16).
fn float_control() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        let mut mxcsr: u32 = 0;
        // SAFETY: `stmxcsr` stores MXCSR into the pointed-to local.
        unsafe {
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut mxcsr, options(nostack, preserves_flags));
        }
        mxcsr as u64
    }
    #[cfg(target_arch = "aarch64")]
    {
        let fpcr: u64;
        // SAFETY: `mrs` copies FPCR into a register.
        unsafe {
            std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
        }
        fpcr
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        0
    }
}

/// Denormal arithmetic the control register would change: a product
/// that underflows, a denormal input, a denormal sum and a tiny vector
/// normalised (all through `black_box`, so nothing folds at compile
/// time).
fn denormals() -> [u32; 4] {
    use std::hint::black_box;
    let tiny = black_box(1e-20f32);
    let sub = black_box(1e-40f32);
    let v = glam::Vec3::new(black_box(1e-23f32), 0.0, 0.0);
    [
        (tiny * tiny).to_bits(),
        (sub * black_box(1.0f32)).to_bits(),
        (sub + sub).to_bits(),
        v.try_normalize().map_or(0, |n| n.x.to_bits()),
    ]
}

fn thread_report(name: &str) -> (String, Vec<u8>) {
    let bits = m0_floatprobe::run_all();
    let d = denormals();
    (
        format!(
            "{name}: control {:#010x}, denormals {:08x} {:08x} {:08x} {:08x}",
            float_control(),
            d[0],
            d[1],
            d[2],
            d[3]
        ),
        bits,
    )
}

/// Byte-slice comparison, copying and sorting against scalar reference
/// loops, at random lengths and alignments (`floatprobe memcmp`). The
/// state hash sorts byte records (`net::state::Records::hash_sorted`),
/// which is libc's `memcmp` under the hood; a Rosetta probe dump showed
/// every hashed value identical to Linux while the hash differed, so
/// the suspicion is the translated `memcmp` / `memcpy` of the x86_64
/// libSystem (CPU-feature dispatched, alignment dependent).
fn memcmp(iters: u64) -> ExitCode {
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let mut rng = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut pool = vec![0u8; 1 << 16];
    for b in &mut pool {
        *b = rng() as u8;
    }
    let scalar_cmp = |a: &[u8], b: &[u8]| -> std::cmp::Ordering {
        for (x, y) in a.iter().zip(b) {
            if x != y {
                return x.cmp(y);
            }
        }
        a.len().cmp(&b.len())
    };
    let (mut bad_cmp, mut bad_eq, mut bad_copy, mut bad_sort) = (0u64, 0u64, 0u64, 0u64);
    let mut shown = 0;
    let mut buf_a = vec![0u8; 1024];
    let mut buf_b = vec![0u8; 1024];
    for i in 0..iters {
        let r = rng();
        let len = (r % 400) as usize;
        let oa = ((r >> 16) % 64) as usize;
        let ob = ((r >> 24) % 64) as usize;
        let src = ((r >> 32) as usize) % (pool.len() - 512);
        // b: a copy of a, with one byte changed at `p` (or none).
        buf_a[oa..oa + len].copy_from_slice(&pool[src..src + len]);
        buf_b[ob..ob + len].copy_from_slice(&pool[src..src + len]);
        let p = ((r >> 40) % 512) as usize;
        let lb = if (r >> 56) & 1 == 1 {
            len.saturating_sub(((r >> 50) % 3) as usize)
        } else {
            len
        };
        if p < lb {
            buf_b[ob + p] ^= 1 | ((r >> 8) as u8 & 0x80);
        }
        let a = &buf_a[oa..oa + len];
        let b = &buf_b[ob..ob + lb];
        // The copies landed intact?
        if a.iter().zip(&pool[src..src + len]).any(|(x, y)| x != y) {
            bad_copy += 1;
        }
        let want = scalar_cmp(a, b);
        if a.cmp(b) != want {
            bad_cmp += 1;
            if shown < 5 {
                shown += 1;
                println!(
                    "cmp mismatch at iter {i}: len {len} / {lb}, offsets {oa} / {ob}, flipped at {p}: got {:?}, scalar {:?}",
                    a.cmp(b),
                    want
                );
            }
        }
        if (a == b) != (want == std::cmp::Ordering::Equal) {
            bad_eq += 1;
        }
    }
    // Sorting records as the state hash does: ~400 records of ~35
    // bytes that share long prefixes, in a buffer at a random offset.
    for round in 0..(iters / 20_000).max(1) {
        let r = rng();
        let n = 300 + (r % 200) as usize;
        let off = ((r >> 16) % 64) as usize;
        let mut buf = vec![0u8; off];
        let mut starts = Vec::with_capacity(n);
        for k in 0..n {
            starts.push(buf.len());
            buf.extend((k as u32 / 3).to_le_bytes());
            let x = rng();
            buf.extend((x as u16).to_le_bytes());
            buf.push((x >> 16) as u8);
            for j in 0..7 {
                buf.extend(((x >> (j * 8)) as f32 * 0.5).to_le_bytes());
            }
        }
        let ends = starts.iter().skip(1).copied().chain([buf.len()]);
        let recs: Vec<&[u8]> = starts.iter().zip(ends).map(|(&a, b)| &buf[a..b]).collect();
        let mut fast = recs.clone();
        fast.sort_unstable();
        let mut slow = recs.clone();
        slow.sort_by(|a, b| scalar_cmp(a, b));
        if fast != slow {
            bad_sort += 1;
            if shown < 10 {
                shown += 1;
                println!("sort mismatch in round {round}: {n} records at offset {off}");
            }
        }
    }
    println!(
        "{iters} comparisons: {bad_cmp} cmp mismatches, {bad_eq} eq mismatches, {bad_copy} bad copies; {bad_sort} sort mismatches"
    );
    if bad_cmp + bad_eq + bad_copy + bad_sort > 0 {
        println!("FAIL: byte comparison, copying or sorting disagrees with the scalar loops");
        ExitCode::FAILURE
    } else {
        println!("OK: memcmp / memcpy / sort agree with the scalar loops");
        ExitCode::SUCCESS
    }
}

/// The byte at `i` of a reference stream: a cheap per-byte formula, so
/// a copy is verified against arithmetic, never against another copy.
fn stream(seed: u64, i: usize) -> u8 {
    ((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seed).rotate_right(29) as u8
}

/// Large copies against the per-byte formula (`floatprobe memcpy`):
/// `copy_from_slice` (memcpy) and `copy_within` (memmove) of up to
/// 256 KiB at random offsets, and `Vec::push` growth (every realloc is
/// a memcpy of the whole buffer). `memcmp` found byte comparison exact
/// under Rosetta; a corrupted byte in a copy would explain a hash that
/// differs over identical values (its scratch buffer grows by realloc)
/// and, in a sim buffer, a position a hair off. On `n` threads at
/// once, as the game's task pools copy concurrently.
fn memcpy(iters: u64, n: usize) -> ExitCode {
    let worker = |t: usize| -> (u64, u64, u64, u64, Vec<String>) {
        let seed = 0xA5A5_0000 + t as u64;
        let mut s = seed | 1;
        let mut rng = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        const MAX: usize = 256 * 1024;
        let src: Vec<u8> = (0..MAX + 128).map(|i| stream(seed, i)).collect();
        let mut dst = vec![0u8; MAX + 128];
        let (mut bad_copy, mut bad_move, mut bad_push, mut copied) = (0u64, 0u64, 0u64, 0u64);
        let mut notes = Vec::new();
        for it in 0..iters {
            let r = rng();
            // Lengths spread over the sizes a Vec passes through.
            let len = match r % 4 {
                0 => (r >> 8) as usize % 256,
                1 => (r >> 8) as usize % 4096,
                2 => (r >> 8) as usize % 65536,
                _ => (r >> 8) as usize % MAX,
            };
            let os = ((r >> 32) % 64) as usize;
            let od = ((r >> 40) % 64) as usize;
            dst[od..od + len].copy_from_slice(&src[os..os + len]);
            copied += len as u64;
            if let Some(i) = (0..len).find(|&i| dst[od + i] != stream(seed, os + i)) {
                bad_copy += 1;
                if notes.len() < 8 {
                    notes.push(format!(
                        "thread {t} iter {it}: memcpy of {len} bytes from offset {os} to {od}: byte {i} is {:#04x}, expected {:#04x}",
                        dst[od + i],
                        stream(seed, os + i)
                    ));
                }
            }
            // memmove within one buffer, overlapping when the offsets
            // are closer than the length.
            let to = ((r >> 48) % 128) as usize;
            dst[od..od + len].copy_from_slice(&src[os..os + len]);
            dst.copy_within(od..od + len, to);
            copied += len as u64;
            if let Some(i) = (0..len).find(|&i| dst[to + i] != stream(seed, os + i)) {
                bad_move += 1;
                if notes.len() < 8 {
                    notes.push(format!(
                        "thread {t} iter {it}: memmove of {len} bytes from {od} to {to}: byte {i} is {:#04x}, expected {:#04x}",
                        dst[to + i],
                        stream(seed, os + i)
                    ));
                }
            }
            // Vec growth: pushes through every doubling up to `len`.
            let mut v: Vec<u8> = Vec::with_capacity(8);
            for i in 0..len {
                v.push(stream(seed, os + i));
            }
            copied += len as u64;
            if let Some(i) = (0..len).find(|&i| v[i] != stream(seed, os + i)) {
                bad_push += 1;
                if notes.len() < 8 {
                    notes.push(format!(
                        "thread {t} iter {it}: Vec of {len} pushed bytes: byte {i} is {:#04x}, expected {:#04x}",
                        v[i],
                        stream(seed, os + i)
                    ));
                }
            }
        }
        (bad_copy, bad_move, bad_push, copied, notes)
    };
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..n).map(|t| s.spawn(move || worker(t))).collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect()
    });
    let (mut bad_copy, mut bad_move, mut bad_push, mut copied) = (0, 0, 0, 0);
    for (c, m, p, bytes, notes) in &results {
        bad_copy += c;
        bad_move += m;
        bad_push += p;
        copied += bytes;
        for note in notes {
            println!("{note}");
        }
    }
    println!(
        "{iters} iterations on {n} threads, {} MiB copied: {bad_copy} bad memcpy, {bad_move} bad memmove, {bad_push} bad Vec growth",
        copied >> 20
    );
    if bad_copy + bad_move + bad_push > 0 {
        println!("FAIL: a copy lost or changed bytes");
        ExitCode::FAILURE
    } else {
        println!("OK: every copy intact");
        ExitCode::SUCCESS
    }
}

/// The game's state hash, byte for byte (`net::state::{Fnv, Records}`):
/// FNV-1a over byte records sorted as byte strings, each after its
/// length. Copied rather than shared so this probe builds alone.
mod game_hash {
    pub struct Fnv(u64);
    impl Default for Fnv {
        fn default() -> Self {
            Self(0xcbf2_9ce4_8422_2325)
        }
    }
    impl Fnv {
        pub fn bytes(&mut self, b: &[u8]) {
            for &x in b {
                self.0 ^= x as u64;
                self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        pub fn finish(self) -> u64 {
            self.0
        }
    }
    #[derive(Default)]
    pub struct Records {
        pub buf: Vec<u8>,
        pub starts: Vec<usize>,
    }
    impl Records {
        pub fn start(&mut self, head: impl AsRef<[u8]>) -> &mut Vec<u8> {
            self.starts.push(self.buf.len());
            self.buf.extend_from_slice(head.as_ref());
            &mut self.buf
        }
        pub fn recs(&self) -> Vec<&[u8]> {
            let ends = self.starts.iter().skip(1).copied().chain([self.buf.len()]);
            self.starts
                .iter()
                .zip(ends)
                .map(|(&a, b)| &self.buf[a..b])
                .collect()
        }
        /// As the game: `sort_unstable` on `Vec<&[u8]>`, then FNV.
        pub fn hash_sorted(&self) -> u64 {
            let mut recs = self.recs();
            recs.sort_unstable();
            fnv(&recs)
        }
        /// The reference: a scalar byte-loop sort, then FNV.
        pub fn hash_sorted_scalar(&self) -> u64 {
            let mut recs = self.recs();
            recs.sort_by(|a, b| {
                for (x, y) in a.iter().zip(b.iter()) {
                    if x != y {
                        return x.cmp(y);
                    }
                }
                a.len().cmp(&b.len())
            });
            fnv(&recs)
        }
    }
    pub fn fnv(recs: &[&[u8]]) -> u64 {
        let mut h = Fnv::default();
        for r in recs {
            h.bytes(&(r.len() as u32).to_le_bytes());
            h.bytes(r);
        }
        h.finish()
    }
}

/// The attempt at a minimal reproducer (`floatprobe stress <secs> <threads>`).
/// Probe run 40371194065 job 3 hashed a tick whose every value equalled
/// Linux's to a different number, so this does what the state hash does,
/// continuously: builds ~570 unit-like records (id, kind, team, seven
/// floats) and hashes them with the game's exact code, twice, and with
/// a scalar reference, and reports any disagreement. With the churn a
/// game process has around it: large allocations freed again (page
/// faults), yields and short sleeps (context switches), on `n` threads.
fn hash_stress(secs: u64, n: usize) -> ExitCode {
    use game_hash::Records;
    use std::sync::atomic::{AtomicU64, Ordering};
    let faults = AtomicU64::new(0);
    let iterations = AtomicU64::new(0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    std::thread::scope(|s| {
        for t in 0..n {
            let (faults, iterations) = (&faults, &iterations);
            s.spawn(move || {
                let mut x = 0x1234_5678_9abc_def0u64 ^ (t as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                let mut rng = move || {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x
                };
                let mut it = 0u64;
                let mut pos: Vec<[f32; 7]> = (0..600)
                    .map(|_| {
                        let r = rng();
                        [
                            (r as u32 % 4096) as f32,
                            ((r >> 16) as u32 % 512) as f32,
                            ((r >> 32) as u32 % 4096) as f32,
                            0.0,
                            std::f32::consts::FRAC_1_SQRT_2,
                            0.0,
                            std::f32::consts::FRAC_1_SQRT_2,
                        ]
                    })
                    .collect();
                while std::time::Instant::now() < deadline {
                    it += 1;
                    let r = rng();
                    let count = 400 + (r % 200) as usize;
                    // Units move a little every tick, as in a match.
                    for p in pos.iter_mut().take(count) {
                        let d = rng();
                        p[0] += ((d as u32 % 200) as f32 - 100.0) * 0.01;
                        p[2] += (((d >> 32) as u32 % 200) as f32 - 100.0) * 0.01;
                        let a = ((d >> 8) as u32 % 65536) as f32 * (std::f32::consts::TAU / 65536.0);
                        p[4] = bevy_math::ops::sin(a * 0.5);
                        p[6] = bevy_math::ops::cos(a * 0.5);
                    }
                    let mut recs = Records::default();
                    for (i, p) in pos.iter().enumerate().take(count) {
                        let me = (i as u32 * 3 + 7).to_le_bytes();
                        let rec = recs.start(me);
                        rec.extend((i as u16 % 40).to_le_bytes());
                        rec.push((i % 8) as u8);
                        for v in p {
                            rec.extend(v.to_le_bytes());
                        }
                    }
                    let h1 = recs.hash_sorted();
                    let h2 = recs.hash_sorted();
                    let h3 = recs.hash_sorted_scalar();
                    if h1 != h2 || h1 != h3 {
                        faults.fetch_add(1, Ordering::Relaxed);
                        println!(
                            "FAULT on thread {t} iteration {it}: game hash {h1:016x}, again {h2:016x}, scalar {h3:016x} ({count} records)"
                        );
                    }
                    // The churn.
                    if it.is_multiple_of(64) {
                        let big = 65536 + (rng() as usize % (8 << 20));
                        let mut v: Vec<u8> = Vec::with_capacity(big);
                        v.resize(big, (it & 0xff) as u8);
                        std::hint::black_box(&v);
                    }
                    if it.is_multiple_of(16) {
                        std::thread::yield_now();
                    }
                    if it.is_multiple_of(256) {
                        std::thread::sleep(std::time::Duration::from_micros(100));
                    }
                }
                iterations.fetch_add(it, Ordering::Relaxed);
            });
        }
        // A progress line a minute, so a job log shows it is alive.
        let mut elapsed = 0;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(
                std::time::Duration::from_secs(60).min(deadline - std::time::Instant::now()),
            );
            elapsed += 1;
            println!(
                "{elapsed} min, {} faults so far",
                faults.load(Ordering::Relaxed)
            );
        }
    });
    let (f, i) = (
        faults.load(Ordering::Relaxed),
        iterations.load(Ordering::Relaxed),
    );
    println!("{secs} s on {n} threads: {i} hashes, {f} faults");
    if f > 0 {
        println!("FAIL: the state hash disagreed with itself or the scalar reference");
        ExitCode::FAILURE
    } else {
        println!("OK: every hash agreed");
        ExitCode::SUCCESS
    }
}

fn threads(n: usize) -> ExitCode {
    let (main_line, main_bits) = thread_report("main");
    println!("{main_line}");
    let others: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..n)
            .map(|i| s.spawn(move || thread_report(&format!("thread {i}"))))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect()
    });
    let mut differs = false;
    for (line, bits) in &others {
        let d: Vec<_> = m0_floatprobe::compare(&main_bits, bits)
            .into_iter()
            .filter(|(_, d)| *d > 0)
            .collect();
        if d.is_empty() {
            println!("{line}: every probe as on main");
        } else {
            differs = true;
            println!("{line}: DIFFERS from main in {d:?}");
        }
    }
    if differs {
        println!("FAIL: a thread's float results differ from the main thread's");
        ExitCode::FAILURE
    } else {
        println!("OK: {n} threads agree with the main thread");
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") if args.len() == 2 => {
            std::fs::write(&args[1], m0_floatprobe::run_all()).expect("write output");
            ExitCode::SUCCESS
        }
        Some("compare") if args.len() >= 3 => {
            let files: Vec<(String, Vec<u8>)> = args[1..]
                .iter()
                .map(|p| (p.clone(), std::fs::read(p).expect("read input")))
                .collect();
            let mut det_differs = false;
            for (i, (na, a)) in files.iter().enumerate() {
                for (nb, b) in &files[i + 1..] {
                    println!("--- {na} vs {nb}");
                    for (name, d) in m0_floatprobe::compare(a, b) {
                        let pct = 100.0 * d as f64 / m0_floatprobe::N as f64;
                        println!(
                            "  {name:40} {d:7} / {} differ ({pct:.3} %)",
                            m0_floatprobe::N
                        );
                        det_differs |= d > 0 && name.starts_with("det:");
                    }
                }
            }
            if det_differs {
                println!("FAIL: a det: probe differs between targets");
                ExitCode::FAILURE
            } else {
                println!("OK: every det: probe is bit-identical");
                ExitCode::SUCCESS
            }
        }
        Some("threads") => threads(args.get(1).and_then(|n| n.parse().ok()).unwrap_or(8)),
        Some("memcmp") => memcmp(
            args.get(1)
                .and_then(|n| n.parse().ok())
                .unwrap_or(20_000_000),
        ),
        Some("stress") => hash_stress(
            args.get(1).and_then(|n| n.parse().ok()).unwrap_or(60),
            args.get(2).and_then(|n| n.parse().ok()).unwrap_or(3),
        ),
        Some("memcpy") => memcpy(
            args.get(1).and_then(|n| n.parse().ok()).unwrap_or(2_000),
            args.get(2).and_then(|n| n.parse().ok()).unwrap_or(4),
        ),
        _ => {
            eprintln!(
                "usage: floatprobe run <out.bin> | floatprobe compare <a.bin> <b.bin> [...] | floatprobe threads [n]"
            );
            ExitCode::FAILURE
        }
    }
}
