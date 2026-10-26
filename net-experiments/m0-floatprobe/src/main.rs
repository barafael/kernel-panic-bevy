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
        _ => {
            eprintln!(
                "usage: floatprobe run <out.bin> | floatprobe compare <a.bin> <b.bin> [...] | floatprobe threads [n]"
            );
            ExitCode::FAILURE
        }
    }
}
