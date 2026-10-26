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
        _ => {
            eprintln!(
                "usage: floatprobe run <out.bin> | floatprobe compare <a.bin> <b.bin> [...] | floatprobe threads [n]"
            );
            ExitCode::FAILURE
        }
    }
}
