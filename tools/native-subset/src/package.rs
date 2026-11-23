//! Reads a `.hmod` (a zip of the mod's source) without extracting it: every entry is checked and
//! read in full, within limits, and only the manifests and Rust files are kept.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Cursor, Read};

use zip::{CompressionMethod, ZipArchive};

use crate::report::{Report, Rule, Side};

/// A package is downloaded and hashed whole: the same limit as the staging of a pull request's file.
pub const MAX_ARCHIVE: u64 = 64 << 20;
pub const MAX_ENTRIES: usize = 20_000;
pub const MAX_ENTRY: u64 = 32 << 20;
/// Uncompressed, every entry together: what an extraction would write.
pub const MAX_TOTAL: u64 = 256 << 20;
pub const MAX_PATH: usize = 240;

/// Files kept in memory: the two manifests, the lock and the Rust sources.
fn kept(name: &str) -> bool {
    matches!(name, "Cargo.toml" | "Cargo.lock" | "horizon-mod.toml") || name.ends_with(".rs")
}

#[derive(Debug, Default)]
pub struct Package {
    /// Every regular file's path.
    pub files: BTreeSet<String>,
    /// The contents of the kept files.
    pub texts: BTreeMap<String, Vec<u8>>,
}

impl Package {
    pub fn text(&self, path: &str) -> Option<&[u8]> {
        self.texts.get(path).map(Vec::as_slice)
    }
}

/// Why a path can't be used, if it can't: the same on every platform the launcher runs on.
pub fn path_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("empty");
    }
    if name.len() > MAX_PATH {
        return Some("too long");
    }
    if name.starts_with('/') {
        return Some("absolute");
    }
    if name.contains('\\') {
        return Some("a backslash");
    }
    if let Some(c) = name.chars().find(|c| !(' '..='~').contains(c)) {
        return Some(if c.is_control() {
            "a control character"
        } else {
            "a character outside printable ASCII"
        });
    }
    if name.contains([':', '*', '?', '"', '<', '>', '|']) {
        return Some("a character Windows refuses");
    }
    for part in name.split('/') {
        match part {
            "" => return Some("an empty component"),
            "." | ".." => return Some("a `.` or `..` component"),
            _ if part.ends_with(['.', ' ']) || part.starts_with(' ') => {
                return Some("a component Windows trims");
            }
            _ => {}
        }
    }
    None
}

fn refuse<T>(report: &mut Report, side: Side, detail: String) -> Option<T> {
    report.add(Rule::Archive, side, "", 0, detail);
    None
}

/// One central directory record, as the bytes say, before any reader's interpretation.
#[derive(Debug)]
pub struct Central {
    pub name: Vec<u8>,
    pub method: u16,
    pub crc: u32,
    pub compressed: u64,
    pub size: u64,
    pub data_start: u64,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Extra fields readers act on are refused, except a Unicode path (0x7075) that names exactly the
/// header's own bytes. Timestamps (0x5455, 0x000a) and owners (0x7875) change nothing read here.
fn extras(extra: &[u8], name: &[u8]) -> Result<(), (Rule, &'static str)> {
    let mut at = 0;
    while at < extra.len() {
        let (Some(id), Some(len)) = (u16_at(extra, at), u16_at(extra, at + 2)) else {
            return Err((Rule::Archive, "a malformed extra field"));
        };
        let Some(data) = extra.get(at + 4..at + 4 + len as usize) else {
            return Err((Rule::Archive, "a malformed extra field"));
        };
        match id {
            0x7075 if data.len() >= 5 && data[0] == 1 && &data[5..] == name => {}
            0x7075 => return Err((Rule::Path, "a Unicode path field that names another file")),
            0x5455 | 0x000a | 0x7875 => {}
            _ => {
                return Err((
                    Rule::Archive,
                    "an extra field readers may act on (zip64, ...)",
                ));
            }
        }
        at += 4 + len as usize;
    }
    Ok(())
}

/// The archive's own structure, read independently of the zip crate, which leaves the local
/// headers unchecked: one end record at the very end, no comments, no zip64, flags only for UTF-8
/// names, and every local header the same as its central record, the entries back to back from the
/// first byte up to the central directory. Anything else could read differently in another tool.
pub fn structure(report: &mut Report, side: Side, b: &[u8]) -> Option<Vec<Central>> {
    let end = b.len().checked_sub(22)?;
    if u32_at(b, end) != Some(0x0605_4b50) {
        return refuse(
            report,
            side,
            "the end record is not the last 22 bytes (a comment or trailing data)".into(),
        );
    }
    let field = |at: usize| u16_at(b, end + at).unwrap_or(0xffff);
    let (count, cd_size, cd_start) = (field(10), u32_at(b, end + 12)?, u32_at(b, end + 16)?);
    if field(4) != 0 || field(6) != 0 || field(8) != count || field(20) != 0 {
        return refuse(
            report,
            side,
            "a multi-disk archive or an archive comment".into(),
        );
    }
    if count == 0xffff || cd_start == u32::MAX || cd_start as u64 + cd_size as u64 != end as u64 {
        return refuse(
            report,
            side,
            "the central directory doesn't end where the end record starts (zip64 or data)".into(),
        );
    }
    let mut at = cd_start as usize;
    let mut entries = Vec::new();
    let mut spans = Vec::new();
    for i in 0..count {
        let shown = format!("#{i}");
        let mut entry_bad = |rule: Rule, detail: &str| {
            report.add(rule, side, &shown, 0, detail);
        };
        let header = (
            u32_at(b, at),
            u16_at(b, at + 6),
            u16_at(b, at + 8),
            u16_at(b, at + 10),
            u32_at(b, at + 16),
        );
        let (Some(0x0201_4b50), Some(needed), Some(flags), Some(method), Some(crc)) = header else {
            entry_bad(Rule::Archive, "a malformed central record");
            return None;
        };
        let (compressed, size) = (u32_at(b, at + 20)?, u32_at(b, at + 24)?);
        let (name_len, extra_len, comment_len) = (
            u16_at(b, at + 28)? as usize,
            u16_at(b, at + 30)? as usize,
            u16_at(b, at + 32)?,
        );
        let (disk, offset) = (u16_at(b, at + 34)?, u32_at(b, at + 42)?);
        let name = b.get(at + 46..at + 46 + name_len)?.to_vec();
        let extra = b.get(at + 46 + name_len..at + 46 + name_len + extra_len)?;
        at += 46 + name_len + extra_len + comment_len as usize;
        if at > end {
            entry_bad(Rule::Archive, "a central record past the central directory");
            return None;
        }
        if flags & !0x0800 != 0 || !matches!(method, 0 | 8) {
            entry_bad(
                Rule::Archive,
                "encryption, a data descriptor or another flag, or a method other than stored or deflated",
            );
            return None;
        }
        if comment_len != 0 || disk != 0 || [compressed, size, offset].contains(&u32::MAX) {
            entry_bad(Rule::Archive, "a file comment, another disk or zip64 sizes");
            return None;
        }
        if let Err((rule, why)) = extras(extra, &name) {
            entry_bad(rule, why);
            return None;
        }
        let o = offset as usize;
        let local = (
            u32_at(b, o),
            u16_at(b, o + 4),
            u16_at(b, o + 6),
            u16_at(b, o + 8),
            u32_at(b, o + 14),
        );
        let local_sizes = (u32_at(b, o + 18), u32_at(b, o + 22));
        let (Some(local_name_len), Some(local_extra_len)) = (u16_at(b, o + 26), u16_at(b, o + 28))
        else {
            entry_bad(Rule::Archive, "a local header outside the archive");
            return None;
        };
        let local_name = b.get(o + 30..o + 30 + local_name_len as usize);
        let local_extra = b.get(
            o + 30 + local_name_len as usize
                ..o + 30 + local_name_len as usize + local_extra_len as usize,
        );
        let same = local
            == (
                Some(0x0403_4b50),
                Some(needed),
                Some(flags),
                Some(method),
                Some(crc),
            )
            && local_sizes == (Some(compressed), Some(size))
            && local_name == Some(name.as_slice());
        if !same {
            entry_bad(
                Rule::Archive,
                "the local header differs from the central record (name, method, flags, CRC or sizes)",
            );
            return None;
        }
        if let Err((rule, why)) = extras(local_extra?, &name) {
            entry_bad(rule, why);
            return None;
        }
        let data_start = (o + 30 + local_name_len as usize + local_extra_len as usize) as u64;
        spans.push((offset as u64, data_start + compressed as u64));
        entries.push(Central {
            name,
            method,
            crc,
            compressed: compressed as u64,
            size: size as u64,
            data_start,
        });
    }
    if at != end {
        return refuse(
            report,
            side,
            "the central directory holds more than its records".into(),
        );
    }
    spans.sort();
    let mut next = 0;
    for (start, stop) in &spans {
        if *start != next {
            return refuse(report, side, "a gap or an overlap between entries".into());
        }
        next = *stop;
    }
    if next != cd_start as u64 {
        return refuse(
            report,
            side,
            "data between the entries and the central directory".into(),
        );
    }
    Some(entries)
}

/// Reads `bytes` as a package. `None` when it can't be read at all; each problem is in `report`.
pub fn read(report: &mut Report, side: Side, bytes: &[u8]) -> Option<Package> {
    if bytes.len() as u64 > MAX_ARCHIVE {
        report.add(
            Rule::Size,
            side,
            "",
            0,
            format!("archive over {MAX_ARCHIVE} bytes"),
        );
        return None;
    }
    let mut zip = match ZipArchive::new(Cursor::new(bytes)) {
        Ok(zip) => zip,
        Err(e) => {
            report.add(
                Rule::Archive,
                side,
                "",
                0,
                format!("not a zip archive: {e}"),
            );
            return None;
        }
    };
    // Data before the archive is skipped by some readers and not by others.
    if zip.offset() != 0 {
        report.add(Rule::Archive, side, "", 0, "data before the archive");
        return None;
    }
    // Readers keep different copies of a name given twice.
    if zip.has_duplicate_names() {
        report.add(Rule::Clash, side, "", 0, "a path given twice");
        return None;
    }
    let before = report.findings.len();
    let Some(central) = structure(report, side, bytes) else {
        if report.findings.len() == before {
            report.add(Rule::Archive, side, "", 0, "not a complete zip archive");
        }
        return None;
    };
    if central.len() != zip.len() {
        report.add(
            Rule::Archive,
            side,
            "",
            0,
            "the zip reader counts other entries than the central directory",
        );
        return None;
    }
    if zip.len() > MAX_ENTRIES {
        report.add(
            Rule::Size,
            side,
            "",
            0,
            format!("over {MAX_ENTRIES} entries"),
        );
        return None;
    }
    let mut package = Package::default();
    let mut folded: HashMap<String, String> = HashMap::new();
    let mut total = 0u64;
    for (i, c) in central.iter().enumerate() {
        let mut entry = match zip.by_index(i) {
            Ok(entry) => entry,
            Err(e) => {
                report.add(
                    Rule::Archive,
                    side,
                    &format!("#{i}"),
                    0,
                    format!("unreadable entry: {e}"),
                );
                return None;
            }
        };
        // What the launcher's reader (this crate) will use must be what the bytes say.
        let method = match entry.compression() {
            CompressionMethod::Stored => 0,
            CompressionMethod::Deflated => 8,
            _ => u16::MAX,
        };
        let agrees = entry.name_raw() == c.name.as_slice()
            && method == c.method
            && entry.crc32() == c.crc
            && entry.compressed_size() == c.compressed
            && entry.size() == c.size
            && entry.data_start() == Some(c.data_start);
        if !agrees {
            report.add(
                Rule::Archive,
                side,
                &format!("#{i}"),
                0,
                "the zip reader disagrees with the headers",
            );
            return None;
        }
        let Ok(name) = String::from_utf8(entry.name_raw().to_vec()) else {
            let shown = String::from_utf8_lossy(entry.name_raw()).into_owned();
            report.add(Rule::Path, side, &shown, 0, "name is not UTF-8");
            continue;
        };
        // A Unicode path extra field can give one name to the reader and another to this check.
        if entry.name().ok().as_deref() != Some(name.as_str()) {
            report.add(
                Rule::Path,
                side,
                &name,
                0,
                "name differs from its extra field",
            );
            continue;
        }
        if entry.encrypted() {
            report.add(Rule::Archive, side, &name, 0, "encrypted");
            continue;
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            report.add(
                Rule::Archive,
                side,
                &name,
                0,
                "compression other than stored or deflated",
            );
            continue;
        }
        let file_type = entry.unix_mode().unwrap_or(0) & 0o170000;
        if entry.is_dir() || entry.is_symlink() || !matches!(file_type, 0 | 0o100000) {
            report.add(
                Rule::EntryKind,
                side,
                &name,
                0,
                format!("not a regular file (type {file_type:o})"),
            );
            continue;
        }
        if let Some(why) = path_problem(&name) {
            report.add(Rule::Path, side, &name, 0, format!("path has {why}"));
            continue;
        }
        if let Some(first) = folded.insert(name.to_ascii_lowercase(), name.clone()) {
            report.add(
                Rule::Clash,
                side,
                &name,
                0,
                format!("same path as {first:?}, ignoring case"),
            );
            continue;
        }
        if entry.size() > MAX_ENTRY {
            report.add(
                Rule::Size,
                side,
                &name,
                0,
                format!("over {MAX_ENTRY} bytes"),
            );
            continue;
        }
        // Read in full, so a CRC or size that doesn't match the data fails here, and within the
        // limit whatever the header claims.
        let mut data = Vec::new();
        if let Err(e) = (&mut entry).take(MAX_ENTRY + 1).read_to_end(&mut data) {
            report.add(Rule::Archive, side, &name, 0, format!("unreadable: {e}"));
            continue;
        }
        if data.len() as u64 != entry.size() {
            report.add(
                Rule::Archive,
                side,
                &name,
                0,
                "size differs from its header",
            );
            continue;
        }
        total += data.len() as u64;
        if total > MAX_TOTAL {
            report.add(
                Rule::Size,
                side,
                &name,
                0,
                format!("entries over {MAX_TOTAL} bytes together"),
            );
            return None;
        }
        if kept(&name) {
            package.texts.insert(name.clone(), data);
        }
        package.files.insert(name);
    }
    // A file named like a directory of another one: extraction writes one over the other.
    for name in &package.files {
        let mut prefix = String::new();
        for part in name
            .split('/')
            .collect::<Vec<_>>()
            .split_last()
            .map_or(&[][..], |(_, dirs)| dirs)
        {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if let Some(file) = folded.get(&prefix.to_ascii_lowercase()) {
                report.add(
                    Rule::Clash,
                    side,
                    name,
                    0,
                    format!("{file:?} is a file, not a directory"),
                );
            }
        }
    }
    Some(package)
}

/// Root files the launcher reads besides docs.
const ROOT_FILES: [&str; 6] = [
    "horizon-mod.toml",
    "Cargo.toml",
    "Cargo.lock",
    "uses.json",
    "layouts.json",
    "icon.png",
];
/// Prefixes of the docs a package may carry at its root.
const ROOT_DOCS: [&str; 4] = ["README", "LICENSE", "NOTICE", "COPYING"];

/// Whether `name` is in the layout: Rust only under `src/`, and nothing Cargo or rustc reads by
/// itself (`build.rs`, `.cargo/`, `rust-toolchain*`, `.horizon/`, `tests/`, ...).
pub fn in_layout(name: &str) -> bool {
    match name.split_once('/') {
        None => {
            ROOT_FILES.contains(&name)
                || (ROOT_DOCS.iter().any(|p| name.starts_with(p))
                    && !name.to_ascii_lowercase().ends_with(".rs"))
        }
        Some(("src", _)) => name.ends_with(".rs"),
        Some(("assets" | "data", _)) => true,
        _ => false,
    }
}

/// The layout of the new package.
pub fn check_layout(report: &mut Report, package: &Package) {
    for name in &package.files {
        if !in_layout(name) {
            report.add(
                Rule::Layout,
                Side::New,
                name,
                0,
                "outside the allowed layout",
            );
        }
    }
    for required in ["horizon-mod.toml", "Cargo.toml", "src/lib.rs"] {
        if !package.files.contains(required) {
            report.add(Rule::Layout, Side::New, required, 0, "missing");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Entry, zip};

    fn read_new(entries: &[Entry]) -> (Report, Option<Package>) {
        let mut r = Report::default();
        let p = read(&mut r, Side::New, &zip(entries));
        (r, p)
    }

    fn rules(entries: &[Entry]) -> Vec<String> {
        let (mut r, p) = read_new(entries);
        if let Some(p) = p {
            check_layout(&mut r, &p);
        }
        r.rules().iter().map(|r| r.id()).collect()
    }

    fn base() -> Vec<Entry> {
        vec![
            Entry::file("horizon-mod.toml", b"id = 'x'"),
            Entry::file("Cargo.toml", b"[package]"),
            Entry::file("src/lib.rs", b"fn a() {}"),
        ]
    }

    fn with(extra: Entry) -> Vec<String> {
        let mut e = base();
        e.push(extra);
        rules(&e)
    }

    #[test]
    fn a_plain_package_reads() {
        let mut e = base();
        e.extend([
            Entry::file("assets/x/lang/en_us.json", b"{}"),
            Entry::file("data/x/recipe/a.json", b"{}"),
            Entry::file("src/a/b.rs", b""),
            Entry::file("LICENSE-MIT", b""),
            Entry::file("README.md", b""),
            Entry::file("NOTICE", b""),
            Entry::file("uses.json", b"{}"),
            Entry::file("icon.png", b""),
            Entry::file("Cargo.lock", b""),
            Entry::file("assets/x/y.rs", b"not compiled"),
            Entry::mode("assets/m.json", b"", 0o600),
        ]);
        let (r, p) = read_new(&e);
        assert!(r.ok(), "{:?}", r.findings);
        let p = p.unwrap();
        assert_eq!(p.files.len(), 14);
        assert_eq!(p.text("src/a/b.rs"), Some(&b""[..]));
        assert_eq!(p.text("assets/x/lang/en_us.json"), None);
        assert!(rules(&e).is_empty());
    }

    #[test]
    fn links_directories_and_devices_are_refused() {
        assert_eq!(
            with(Entry::mode("src/link.rs", b"/etc/passwd", 0o120777)),
            ["R3"]
        );
        assert_eq!(with(Entry::mode("assets/dir/", b"", 0o040755)), ["R3"]);
        assert_eq!(with(Entry::file("assets/dir/", b"")), ["R3"]);
        assert_eq!(with(Entry::mode("assets/fifo", b"", 0o010644)), ["R3"]);
    }

    #[test]
    fn traversal_and_unportable_paths_are_refused() {
        for name in [
            "../evil.rs",
            "src/../../evil.rs",
            "/etc/passwd",
            "src\\..\\x.rs",
            "assets//x",
            "assets/./x",
            "assets/x\u{0}y",
            "assets/x\ny",
            "assets/é",
            "assets/C:x",
            "assets/x.",
            "assets/x /y",
        ] {
            assert_eq!(with(Entry::file(name, b"")), ["R4"], "{name:?}");
        }
        assert_eq!(
            with(Entry::raw(b"assets/\xff".to_vec(), b"", 0o100644)),
            ["R4"]
        );
    }

    #[test]
    fn duplicate_and_case_clashing_names_are_refused() {
        assert_eq!(with(Entry::file("src/lib.rs", b"fn b() {}")), ["R5"]);
        assert_eq!(with(Entry::file("SRC/LIB.rs", b"")), ["R5"]);
        assert_eq!(with(Entry::file("cargo.toml", b"")), ["R5"]);
        // A file and a directory of the same name, ignoring case.
        let mut e = base();
        e.extend([Entry::file("assets/a", b""), Entry::file("assets/A/b", b"")]);
        assert_eq!(rules(&e), ["R5"]);
    }

    #[test]
    fn paths_outside_the_layout_are_refused() {
        for name in [
            "build.rs",
            ".cargo/config.toml",
            "rust-toolchain.toml",
            ".horizon/sdk/api/crates/horizon-mod-api/Cargo.toml",
            "tests/a.rs",
            "examples/a.rs",
            "src/rustfmt.toml",
            "src/data.json",
            "LICENSE.rs",
            "other.txt",
            "mixins/a.rs",
        ] {
            assert_eq!(with(Entry::file(name, b"")), ["R6"], "{name:?}");
        }
        assert_eq!(rules(&base()[..2]), ["R6"]);
    }

    #[test]
    fn bombs_and_bad_archives_are_refused() {
        let mut r = Report::default();
        assert!(read(&mut r, Side::New, b"not a zip").is_none());
        assert_eq!(r.rules(), [Rule::Archive]);

        let mut r = Report::default();
        let mut prefixed = b"MZ".to_vec();
        prefixed.extend(zip(&base()));
        assert!(read(&mut r, Side::New, &prefixed).is_none() || !r.ok());
        assert_eq!(r.rules(), [Rule::Archive]);

        let mut r = Report::default();
        assert!(read(&mut r, Side::New, &vec![0u8; MAX_ARCHIVE as usize + 1]).is_none());
        assert_eq!(r.rules(), [Rule::Size]);

        // A header that claims more than the limit, and one that lies about its size.
        assert_eq!(
            with(Entry::sized("assets/big", b"x", MAX_ENTRY + 1)),
            ["R7"]
        );
        assert_eq!(with(Entry::sized("assets/lie", b"abcdef", 2)), ["R2"]);
        assert_eq!(with(Entry::crc("assets/crc", b"abc", 0xdead_beef)), ["R2"]);
        let many: Vec<Entry> = (0..=MAX_ENTRIES)
            .map(|i| Entry::file(&format!("assets/{i}"), b""))
            .collect();
        assert_eq!(rules(&many), ["R7"]);
    }

    fn one(e: Entry) -> Vec<String> {
        with(e)
    }

    #[test]
    fn local_headers_must_match_the_central_directory() {
        let a = || Entry::file("assets/a", b"xyz");
        assert_eq!(one(a().with(|e| e.set_local_name("assets/b"))), ["R2"]);
        assert_eq!(one(a().with(|e| e.set_local_name("src/evil.rs"))), ["R2"]);
        assert_eq!(one(a().with(|e| e.set_local_crc(1))), ["R2"]);
        assert_eq!(one(a().with(|e| e.set_local_method(8))), ["R2"]);
        // A data descriptor, encryption, or an unknown flag.
        for flags in [0x0808, 0x0801, 0x0802, 0x2000] {
            assert_eq!(one(a().with(|e| e.set_flags(flags))), ["R2"], "{flags:x}");
        }
        assert!(one(a().with(|e| e.set_flags(0))).is_empty());
    }

    #[test]
    fn unicode_path_fields_must_name_the_header_itself() {
        let a = || Entry::file("assets/a", b"xyz");
        for (central, local) in [(true, false), (false, true), (true, true)] {
            let e = a().with(|e| e.unicode_path("src/evil.rs", central, local));
            assert_eq!(one(e), ["R4"], "{central} {local}");
        }
        assert!(one(a().with(|e| e.unicode_path("assets/a", true, true))).is_empty());
        // zip64 and unknown fields are refused, a timestamp isn't; a truncated field is refused.
        assert_eq!(one(a().with(|e| e.raw_extra(&[1, 0, 0, 0]))), ["R2"]);
        assert_eq!(one(a().with(|e| e.raw_extra(&[0x99, 0x99, 0, 0]))), ["R2"]);
        assert!(one(a().with(|e| e.raw_extra(&[0x55, 0x54, 1, 0, 0]))).is_empty());
        assert_eq!(
            one(a().with(|e| e.raw_extra(&[0x55, 0x54, 9, 0, 0]))),
            ["R2"]
        );
    }

    #[test]
    fn comments_trailing_data_and_gaps_are_refused() {
        let rules_of = |bytes: &[u8]| {
            let mut r = Report::default();
            let read_ok = read(&mut r, Side::New, bytes).is_some();
            (
                read_ok,
                r.rules().iter().map(|r| r.id()).collect::<Vec<_>>(),
            )
        };
        let z = zip(&base());
        assert_eq!(rules_of(&z), (true, vec![]));
        let mut trailing = z.clone();
        trailing.extend(b"junk");
        assert_eq!(rules_of(&trailing), (false, vec!["R2".to_owned()]));
        let mut comment = z.clone();
        let n = comment.len();
        comment[n - 2..].copy_from_slice(&4u16.to_le_bytes());
        comment.extend(b"note");
        assert_eq!(rules_of(&comment), (false, vec!["R2".to_owned()]));
        // Four hidden bytes before the central directory, which the end record still finds.
        let end = z.len() - 22;
        let start = u32::from_le_bytes(z[end + 16..end + 20].try_into().unwrap()) as usize;
        let mut gap = z[..start].to_vec();
        gap.extend(b"hide");
        gap.extend(&z[start..]);
        let end = gap.len() - 22;
        gap[end + 16..end + 20].copy_from_slice(&(start as u32 + 4).to_le_bytes());
        assert_eq!(rules_of(&gap), (false, vec!["R2".to_owned()]));
    }

    #[test]
    fn a_deflated_bomb_stops_at_the_limit() {
        let big = vec![0u8; (MAX_ENTRY + 1) as usize];
        // The header claims 10 bytes; the data inflates to more than the limit.
        let e = Entry::deflated("assets/bomb", &big).claiming(10);
        assert_eq!(with(e), ["R2"]);
    }
}
