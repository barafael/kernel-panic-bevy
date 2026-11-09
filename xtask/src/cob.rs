//! `cargo xtask cob <file.cob>`: the bytecode of a compiled Spring unit
//! script, disassembled. The disassembler is the `cobdump` example of the
//! retired `spring-cob` crate (the real opcode table and parser, with
//! angles in degrees and distances in elmos); that crate is outside the
//! workspace, so this runs it from there.

use std::path::Path;
use std::process::Command;

pub fn run(args: &[String]) -> Result<(), String> {
    let [file] = args else {
        return Err("usage: cargo xtask cob <file.cob>".into());
    };
    // The example runs in the crate's directory, so a relative path has to
    // be made absolute first.
    let file = std::path::absolute(Path::new(file)).map_err(|e| format!("{file}: {e}"))?;
    let status = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["run", "--quiet", "--example", "cobdump", "--"])
        .arg(file)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../spring-cob"))
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "cobdump failed".into())
}
