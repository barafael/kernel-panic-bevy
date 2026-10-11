//! `floatprobe run <out.bin>`: write every probe's result bits.
//! `floatprobe compare <a.bin> <b.bin> [...]`: per-probe differing inputs
//! for each pair; exits 1 if any `det:` probe differs.

use std::process::ExitCode;

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
        _ => {
            eprintln!("usage: floatprobe run <out.bin> | floatprobe compare <a.bin> <b.bin> [...]");
            ExitCode::FAILURE
        }
    }
}
