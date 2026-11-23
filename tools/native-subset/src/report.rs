//! Findings and their stable rule ids. The workflow reads only `ok` and `rules`; the ids and their
//! meaning below never change once published, so a comment that names one stays true.

use std::fmt::Write;

/// Every rule, in id order. A refused package lists the ids of the rules it broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    /// A package's SHA-256 differs from its entry.
    Hash = 1,
    /// Not a readable zip: prefixed data, an encrypted or unsupported entry, a failed CRC.
    Archive,
    /// An entry that is not a regular file: a link, a directory, a device.
    EntryKind,
    /// An unsafe or unportable path: absolute, `..`, `\`, a control character.
    Path,
    /// Two paths that are the same ignoring case, or a file where a directory is.
    Clash,
    /// A path outside the layout: manifests, `src/**/*.rs`, `assets/`, `data/`, docs.
    Layout,
    /// Over a size limit: the archive, an entry, their total, or the number of entries.
    Size,
    /// `Cargo.toml` changes more than `package.version` and `package.description`.
    CargoChanged,
    /// `Cargo.toml` asks for more than the SDK: another dependency, a build script, a proc macro.
    CargoBuild,
    /// `Cargo.lock` is new or changes more than the mod's own version.
    Lock,
    /// `horizon-mod.toml` has native tables, `internals`, injections or `wasm`.
    NativeTables,
    /// `horizon-mod.toml` changes more than its version, description and pins, or names another id
    /// or version than the entry.
    ManifestChanged,
    /// The `horizon` pin has no SDK release, or `mixins` is not that release's horizon-mixins.
    Pin,
    /// A Rust file does not parse, is not UTF-8, or uses syntax the checker can't read.
    Parse,
    /// `unsafe`.
    Unsafe,
    /// `extern`: an ABI, an extern block or an extern crate.
    Extern,
    /// `macro_rules!`.
    MacroRules,
    /// An attribute outside the allowlist.
    Attribute,
    /// A macro outside the allowlist.
    Macro,
    /// `include_str!`/`include_bytes!` of something other than a non-Rust file of the package by a
    /// plain relative path.
    Include,
    /// A `std`, `core` or `alloc` module outside the allowlist.
    StdModule,
    /// A rename or glob import of `std`, `core` or `alloc` itself.
    StdRoot,
}

impl Rule {
    pub const ALL: [Rule; 22] = [
        Rule::Hash,
        Rule::Archive,
        Rule::EntryKind,
        Rule::Path,
        Rule::Clash,
        Rule::Layout,
        Rule::Size,
        Rule::CargoChanged,
        Rule::CargoBuild,
        Rule::Lock,
        Rule::NativeTables,
        Rule::ManifestChanged,
        Rule::Pin,
        Rule::Parse,
        Rule::Unsafe,
        Rule::Extern,
        Rule::MacroRules,
        Rule::Attribute,
        Rule::Macro,
        Rule::Include,
        Rule::StdModule,
        Rule::StdRoot,
    ];

    pub fn id(self) -> String {
        format!("R{}", self as u8)
    }
}

/// Which package a finding is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Old,
    New,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub rule: Rule,
    pub side: Side,
    pub path: String,
    pub line: usize,
    pub detail: String,
}

/// The most findings kept: enough to read, and the output stays small whatever the package holds.
const MAX_FINDINGS: usize = 200;

#[derive(Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    dropped: usize,
}

impl Report {
    pub fn add(
        &mut self,
        rule: Rule,
        side: Side,
        path: &str,
        line: usize,
        detail: impl Into<String>,
    ) {
        let finding = Finding {
            rule,
            side,
            path: path.to_owned(),
            line,
            detail: detail.into(),
        };
        // The tree and the token pass can both see one macro.
        if self.findings.contains(&finding) {
            return;
        }
        if self.findings.len() < MAX_FINDINGS {
            self.findings.push(finding);
        } else {
            self.dropped += 1;
        }
    }

    pub fn ok(&self) -> bool {
        self.findings.is_empty()
    }

    /// The broken rules, each once, in id order.
    pub fn rules(&self) -> Vec<Rule> {
        let mut rules: Vec<Rule> = self.findings.iter().map(|f| f.rule).collect();
        rules.sort();
        rules.dedup();
        rules
    }

    pub fn to_json(&self) -> String {
        let mut out = String::new();
        let rules: Vec<String> = self
            .rules()
            .iter()
            .map(|r| format!("\"{}\"", r.id()))
            .collect();
        let _ = write!(
            out,
            "{{\"ok\":{},\"rules\":[{}],\"findings\":[",
            self.ok(),
            rules.join(",")
        );
        for (i, f) in self.findings.iter().enumerate() {
            let side = match f.side {
                Side::Old => "old",
                Side::New => "new",
            };
            let _ = write!(
                out,
                "{}{{\"rule\":\"{}\",\"package\":\"{side}\",\"path\":{},\"line\":{},\"detail\":{}}}",
                if i == 0 { "" } else { "," },
                f.rule.id(),
                quote(&f.path),
                f.line,
                quote(&f.detail)
            );
        }
        let _ = write!(out, "],\"dropped\":{}}}", self.dropped);
        out
    }
}

/// A JSON string. Everything outside printable ASCII is escaped, so a package's names can't add a
/// line, a terminal escape or a workflow command to whatever prints this.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_in_order() {
        for (i, rule) in Rule::ALL.iter().enumerate() {
            assert_eq!(rule.id(), format!("R{}", i + 1));
        }
        assert_eq!(Rule::Hash.id(), "R1");
        assert_eq!(Rule::Pin.id(), "R13");
        assert_eq!(Rule::StdRoot.id(), "R22");
    }

    #[test]
    fn json_escapes_everything_outside_printable_ascii() {
        let mut r = Report::default();
        r.add(
            Rule::Path,
            Side::New,
            "a\n::set-output name=pr::1\u{1b}é\"\\",
            3,
            "x",
        );
        r.add(Rule::Hash, Side::Old, "", 0, "y");
        let json = r.to_json();
        assert!(!json.contains('\n') && !json.contains('\u{1b}') && json.is_ascii());
        assert!(json.starts_with("{\"ok\":false,\"rules\":[\"R1\",\"R4\"],"));
        assert!(json.contains(r#""path":"a\u000a::set-output name=pr::1\u001b\u00e9\"\\""#));
    }

    #[test]
    fn findings_are_capped() {
        let mut r = Report::default();
        r.add(Rule::Unsafe, Side::New, "src/lib.rs", 0, "unsafe");
        r.add(Rule::Unsafe, Side::New, "src/lib.rs", 0, "unsafe");
        assert_eq!(r.findings.len(), 1, "the same finding twice is one");
        for i in 1..MAX_FINDINGS + 5 {
            r.add(Rule::Unsafe, Side::New, "src/lib.rs", i, "unsafe");
        }
        assert_eq!(r.findings.len(), MAX_FINDINGS);
        assert!(r.to_json().ends_with("\"dropped\":5}"));
    }
}
