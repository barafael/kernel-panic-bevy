use std::io::Write;
fn main() {
    let out = std::env::args().nth(1).expect("out file");
    let mut buf = Vec::new();
    for f in 0..floatprobe::FUNCS {
        for i in 0..floatprobe::N {
            buf.extend(floatprobe::eval(f, i).to_le_bytes());
        }
    }
    std::fs::File::create(out).unwrap().write_all(&buf).unwrap();
}
