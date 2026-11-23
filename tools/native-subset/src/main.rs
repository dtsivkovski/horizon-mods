//! `native-subset --id <id> --version <version> --old <file> --old-sha256 <hex> --new <file>
//! --new-sha256 <hex> --pin <horizon>=<mixins|-> ...`
//!
//! Prints one JSON object: `ok`, `rules` (stable ids, R1 to R22) and `findings`. Exits 0 when the
//! update is in the subset, 1 when it is not, 2 when the arguments are wrong.
use std::io::Read;
use std::process::ExitCode;

use native_subset::{Update, check, package::MAX_ARCHIVE};

fn hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// At most one byte over the limit, so an oversized file is refused without reading it all.
fn read(path: &str) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_ARCHIVE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{path}: {e}"))?;
    Ok(bytes)
}

struct Args {
    id: String,
    version: String,
    old: String,
    old_sha256: String,
    new: String,
    new_sha256: String,
    pins: Vec<(String, Option<String>)>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        id: String::new(),
        version: String::new(),
        old: String::new(),
        old_sha256: String::new(),
        new: String::new(),
        new_sha256: String::new(),
        pins: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--id" => a.id = value,
            "--version" => a.version = value,
            "--old" => a.old = value,
            "--old-sha256" => a.old_sha256 = value,
            "--new" => a.new = value,
            "--new-sha256" => a.new_sha256 = value,
            "--pin" => {
                let (horizon, mixins) = value
                    .split_once('=')
                    .ok_or("--pin is <horizon>=<mixins|->")?;
                if !hex(horizon, 40) || !(mixins == "-" || hex(mixins, 40)) {
                    return Err("--pin takes 40-digit lowercase commits".into());
                }
                a.pins
                    .push((horizon.into(), (mixins != "-").then(|| mixins.into())));
            }
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    let id_ok = !a.id.is_empty()
        && a.id.len() <= 64
        && a.id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !id_ok || a.version.is_empty() || a.old.is_empty() || a.new.is_empty() {
        return Err("--id, --version, --old and --new are required".into());
    }
    if !hex(&a.old_sha256, 64) || !hex(&a.new_sha256, 64) {
        return Err("--old-sha256 and --new-sha256 take 64 lowercase hex digits".into());
    }
    Ok(a)
}

fn main() -> ExitCode {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("native-subset: {e}");
            return ExitCode::from(2);
        }
    };
    let (old, new) = match (read(&a.old), read(&a.new)) {
        (Ok(old), Ok(new)) => (old, new),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("native-subset: {e}");
            return ExitCode::from(2);
        }
    };
    let update = Update {
        id: &a.id,
        version: &a.version,
        old: &old,
        old_sha256: &a.old_sha256,
        new: &new,
        new_sha256: &a.new_sha256,
        pins: &a.pins,
    };
    let report = check(&update);
    println!("{}", report.to_json());
    if report.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
