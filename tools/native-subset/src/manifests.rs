//! The build inputs: `Cargo.toml`, `Cargo.lock` and `horizon-mod.toml` of the new package, each
//! compared with the previous release's, which a reviewer read.

use toml::{Table, Value};

use crate::Update;
use crate::package::Package;
use crate::report::{Report, Rule, Side};

/// The one dependency, at the path where the launcher puts its own signed SDK.
pub const SDK_DEPENDENCY: &str = "horizon-mod-api";
pub const SDK_PATH: &str = ".horizon/sdk/api/crates/horizon-mod-api";

/// Not `[workspace]`, `[patch]` or `[replace]`, which the launcher refuses (`crates.rs`'s `wanted`).
const CARGO_TABLES: [&str; 4] = ["package", "dependencies", "lib", "lints"];
/// Descriptive `[package]` keys, and `build` only as `false`.
const PACKAGE_KEYS: [&str; 15] = [
    "name",
    "version",
    "edition",
    "license",
    "description",
    "publish",
    "rust-version",
    "authors",
    "readme",
    "repository",
    "homepage",
    "documentation",
    "keywords",
    "categories",
    "build",
];
/// `horizon-mod.toml` keys that compile or link more than the mod's crate against the SDK.
const NATIVE_ONLY: [&str; 4] = ["internals", "inject", "injection", "wasm"];
/// The `[native]` keys of a mod that has no mixins, modules, bridges, state, loops or world control.
const NATIVE_KEYS: [&str; 3] = ["game", "horizon", "mixins"];

fn parse(
    report: &mut Report,
    rule: Rule,
    side: Side,
    package: &Package,
    path: &str,
) -> Option<Table> {
    let Some(bytes) = package.text(path) else {
        report.add(rule, side, path, 0, "missing");
        return None;
    };
    let parsed = std::str::from_utf8(bytes)
        .map_err(|e| e.to_string())
        .and_then(|s| s.parse::<Table>().map_err(|e| e.to_string()));
    match parsed {
        Ok(table) => Some(table),
        Err(e) => {
            report.add(
                rule,
                side,
                path,
                0,
                format!("doesn't parse: {}", e.lines().next().unwrap_or("")),
            );
            None
        }
    }
}

/// `table` without the keys at each of `paths` (dotted, in tables only).
fn without(table: &Table, paths: &[&[&str]]) -> Table {
    fn remove(table: &mut Table, path: &[&str]) {
        match path {
            [] => {}
            [last] => {
                table.remove(*last);
            }
            [first, rest @ ..] => {
                if let Some(Value::Table(t)) = table.get_mut(*first) {
                    remove(t, rest);
                }
            }
        }
    }
    let mut table = table.clone();
    for path in paths {
        remove(&mut table, path);
    }
    table
}

/// The top-level keys (and `[package]`, `[native]` keys) where `a` and `b` differ.
fn differences(a: &Table, b: &Table) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in a.keys().chain(b.keys()) {
        if a.get(key) == b.get(key) || keys.contains(key) {
            continue;
        }
        match (a.get(key), b.get(key)) {
            (Some(Value::Table(x)), Some(Value::Table(y))) => {
                for sub in differences(x, y) {
                    let path = format!("{key}.{sub}");
                    if !keys.contains(&path) {
                        keys.push(path);
                    }
                }
            }
            _ => keys.push(key.clone()),
        }
    }
    keys
}

fn string<'a>(table: &'a Table, path: &[&str]) -> Option<&'a str> {
    let (last, parents) = path.split_last()?;
    let mut at = table;
    for key in parents {
        at = at.get(*key)?.as_table()?;
    }
    at.get(*last)?.as_str()
}

pub fn check(report: &mut Report, old: &Package, new: &Package, update: &Update) {
    let old_cargo = parse(report, Rule::CargoChanged, Side::Old, old, "Cargo.toml");
    let new_cargo = parse(report, Rule::CargoChanged, Side::New, new, "Cargo.toml");
    if let Some(cargo) = &new_cargo {
        cargo_rules(report, cargo);
    }
    if let (Some(o), Some(n)) = (&old_cargo, &new_cargo) {
        let versionless: &[&[&str]] = &[&["package", "version"], &["package", "description"]];
        let changed = differences(&without(o, versionless), &without(n, versionless));
        if !changed.is_empty() {
            report.add(
                Rule::CargoChanged,
                Side::New,
                "Cargo.toml",
                0,
                format!("changes {}", changed.join(", ")),
            );
        }
        lock(report, old, new, o, n);
    }

    let old_manifest = parse(
        report,
        Rule::ManifestChanged,
        Side::Old,
        old,
        "horizon-mod.toml",
    );
    let Some(manifest) = parse(
        report,
        Rule::ManifestChanged,
        Side::New,
        new,
        "horizon-mod.toml",
    ) else {
        return;
    };
    manifest_rules(report, &manifest);
    if let Some(o) = &old_manifest {
        let open: &[&[&str]] = &[
            &["version"],
            &["description"],
            &["native", "horizon"],
            &["native", "mixins"],
        ];
        let changed = differences(&without(o, open), &without(&manifest, open));
        if !changed.is_empty() {
            report.add(
                Rule::ManifestChanged,
                Side::New,
                "horizon-mod.toml",
                0,
                format!("changes {}", changed.join(", ")),
            );
        }
    }
    if string(&manifest, &["id"]) != Some(update.id) {
        report.add(
            Rule::ManifestChanged,
            Side::New,
            "horizon-mod.toml",
            0,
            "id differs from the entry's",
        );
    }
    if string(&manifest, &["version"]) != Some(update.version) {
        report.add(
            Rule::ManifestChanged,
            Side::New,
            "horizon-mod.toml",
            0,
            "version differs from the entry's",
        );
    }
    pins(report, &manifest, update.pins);
}

/// What the new `Cargo.toml` may ask Cargo for, whatever the old one did.
pub fn cargo_rules(report: &mut Report, cargo: &Table) {
    let mut refuse =
        |detail: String| report.add(Rule::CargoBuild, Side::New, "Cargo.toml", 0, detail);
    for key in cargo.keys() {
        if !CARGO_TABLES.contains(&key.as_str()) {
            refuse(format!("[{key}] is not allowed"));
        }
    }
    match cargo.get("package") {
        Some(Value::Table(package)) => {
            for (key, value) in package {
                if !PACKAGE_KEYS.contains(&key.as_str()) {
                    refuse(format!("package.{key} is not allowed"));
                } else if key == "build" && value != &Value::Boolean(false) {
                    refuse("a build script".into());
                }
            }
        }
        _ => refuse("no [package]".into()),
    }
    if let Some(lib) = cargo.get("lib") {
        let Some(lib) = lib.as_table() else {
            return refuse("[lib] is not a table".into());
        };
        for (key, value) in lib {
            match key.as_str() {
                "path" if value.as_str() == Some("src/lib.rs") => {}
                "proc-macro" | "proc_macro" => refuse("a proc-macro crate".into()),
                _ => refuse(format!("lib.{key} is not allowed")),
            }
        }
    }
    let sdk_only = cargo
        .get("dependencies")
        .and_then(Value::as_table)
        .is_some_and(|deps| {
            deps.len() == 1
                && deps
                    .get(SDK_DEPENDENCY)
                    .and_then(Value::as_table)
                    .is_some_and(|dep| {
                        dep.len() == 1 && dep.get("path").and_then(Value::as_str) == Some(SDK_PATH)
                    })
        });
    if !sdk_only {
        refuse(format!(
            "dependencies other than {SDK_DEPENDENCY} at {SDK_PATH}"
        ));
    }
}

/// `Cargo.lock` is absent from the new package, or the old one with only the mod's own version
/// changed.
fn lock(report: &mut Report, old: &Package, new: &Package, old_cargo: &Table, new_cargo: &Table) {
    if new.text("Cargo.lock").is_none() {
        return;
    }
    if old.text("Cargo.lock").is_none() {
        report.add(Rule::Lock, Side::New, "Cargo.lock", 0, "added");
        return;
    }
    let (Some(o), Some(n)) = (
        parse(report, Rule::Lock, Side::Old, old, "Cargo.lock"),
        parse(report, Rule::Lock, Side::New, new, "Cargo.lock"),
    ) else {
        return;
    };
    let name = string(new_cargo, &["package", "name"]);
    let (Some(old_version), Some(new_version)) = (
        string(old_cargo, &["package", "version"]),
        string(new_cargo, &["package", "version"]),
    ) else {
        report.add(
            Rule::Lock,
            Side::New,
            "Cargo.lock",
            0,
            "no package version to compare",
        );
        return;
    };
    let mut n = n;
    if let Some(Value::Array(packages)) = n.get_mut("package") {
        for package in packages.iter_mut().filter_map(Value::as_table_mut) {
            let own = package.get("name").and_then(Value::as_str) == name
                && package.get("version").and_then(Value::as_str) == Some(new_version)
                && !package.contains_key("source");
            if own {
                package.insert("version".into(), Value::String(old_version.into()));
            }
        }
    }
    if n != o {
        report.add(
            Rule::Lock,
            Side::New,
            "Cargo.lock",
            0,
            "changes more than the mod's own version",
        );
    }
}

/// What the new `horizon-mod.toml` may ask the launcher for, whatever the old one did.
pub fn manifest_rules(report: &mut Report, manifest: &Table) {
    for key in NATIVE_ONLY {
        if manifest.contains_key(key) {
            report.add(
                Rule::NativeTables,
                Side::New,
                "horizon-mod.toml",
                0,
                format!("{key} is not allowed"),
            );
        }
    }
    match manifest.get("native") {
        Some(Value::Table(native)) => {
            for (key, value) in native {
                if !NATIVE_KEYS.contains(&key.as_str()) {
                    report.add(
                        Rule::NativeTables,
                        Side::New,
                        "horizon-mod.toml",
                        0,
                        format!("native.{key} is not allowed"),
                    );
                } else if !value.is_str() {
                    report.add(
                        Rule::NativeTables,
                        Side::New,
                        "horizon-mod.toml",
                        0,
                        format!("native.{key} is not a string"),
                    );
                }
            }
        }
        _ => report.add(
            Rule::ManifestChanged,
            Side::New,
            "horizon-mod.toml",
            0,
            "no [native] table",
        ),
    }
}

/// The `horizon` pin is a commit with an SDK release, and `mixins` is the revision that commit
/// builds: never a fork's commit.
pub fn pins(report: &mut Report, manifest: &Table, released: &[(String, Option<String>)]) {
    let horizon = string(manifest, &["native", "horizon"]);
    let Some((_, engine_mixins)) = released
        .iter()
        .find(|(commit, _)| Some(commit.as_str()) == horizon)
    else {
        report.add(
            Rule::Pin,
            Side::New,
            "horizon-mod.toml",
            0,
            "native.horizon is not a released SDK commit",
        );
        return;
    };
    let mixins = manifest
        .get("native")
        .and_then(Value::as_table)
        .and_then(|n| n.get("mixins"));
    if let Some(mixins) = mixins
        && (engine_mixins.is_none() || mixins.as_str() != engine_mixins.as_deref())
    {
        report.add(
            Rule::Pin,
            Side::New,
            "horizon-mod.toml",
            0,
            "native.mixins is not the engine's horizon-mixins revision",
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::{Entry, zip};
    use crate::{Update, check};

    pub const H1: &str = "1111111111111111111111111111111111111111";
    pub const H2: &str = "2222222222222222222222222222222222222222";
    pub const M1: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    pub const M2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn manifest(version: &str, horizon: &str, mixins: &str, extra: &str) -> String {
        format!(
            "id = \"demo\"\nname = \"Demo\"\nversion = \"{version}\"\ndescription = \"Demo {version}\"\n\
             authors = [\"a\"]\napi = \"0.2\"\nside = \"both\"\ncapabilities = [\"registries\"]\n{extra}\n\
             [native]\ngame = \"26.3\"\nhorizon = \"{horizon}\"\nmixins = \"{mixins}\"\n"
        )
    }

    fn cargo(version: &str, extra: &str) -> String {
        format!(
            "[package]\nname = \"demo\"\nversion = \"{version}\"\nedition = \"2024\"\ndescription = \"Demo {version}\"\n\
             publish = false\n{extra}\n[dependencies]\nhorizon-mod-api = {{ path = \".horizon/sdk/api/crates/horizon-mod-api\" }}\n"
        )
    }

    fn lock(version: &str) -> String {
        format!(
            "version = 4\n\n[[package]]\nname = \"demo\"\nversion = \"{version}\"\ndependencies = [\"horizon-mod-api\"]\n\n\
             [[package]]\nname = \"horizon-mod-api\"\nversion = \"0.2.0\"\n"
        )
    }

    struct Pair {
        old: Vec<(String, String)>,
        new: Vec<(String, String)>,
    }

    impl Pair {
        fn new() -> Pair {
            let files = |v: &str, h: &str, m: &str| {
                vec![
                    ("horizon-mod.toml".to_owned(), manifest(v, h, m, "")),
                    ("Cargo.toml".to_owned(), cargo(v, "")),
                    (
                        "src/lib.rs".to_owned(),
                        "pub fn a() -> u32 { 1 }\n".to_owned(),
                    ),
                ]
            };
            Pair {
                old: files("0.1.0", H1, M1),
                new: files("0.2.0", H2, M2),
            }
        }
        fn set(mut self, new: bool, path: &str, text: &str) -> Pair {
            let files = if new { &mut self.new } else { &mut self.old };
            files.retain(|(p, _)| p != path);
            files.push((path.to_owned(), text.to_owned()));
            self
        }
        fn rules(&self) -> Vec<String> {
            let z = |files: &[(String, String)]| {
                zip(&files
                    .iter()
                    .map(|(p, t)| Entry::file(p, t.as_bytes()))
                    .collect::<Vec<_>>())
            };
            let (old, new) = (z(&self.old), z(&self.new));
            let (os, ns) = (crate::sha256_hex(&old), crate::sha256_hex(&new));
            let pins = [
                (H1.to_owned(), Some(M1.to_owned())),
                (H2.to_owned(), Some(M2.to_owned())),
            ];
            let update = Update {
                id: "demo",
                version: "0.2.0",
                old: &old,
                old_sha256: &os,
                new: &new,
                new_sha256: &ns,
                pins: &pins,
            };
            let r = check(&update);
            r.rules().iter().map(|r| r.id()).collect()
        }
    }

    #[test]
    fn a_version_bump_passes() {
        assert!(Pair::new().rules().is_empty());
        // The pins may stay as they were.
        let same = Pair::new().set(true, "horizon-mod.toml", &manifest("0.2.0", H1, M1, ""));
        assert!(same.rules().is_empty());
        // Comments and layout of the manifests don't count.
        let p = Pair::new().set(
            true,
            "Cargo.toml",
            &format!("# a comment\n{}", cargo("0.2.0", "")),
        );
        assert!(p.rules().is_empty());
        let p = Pair::new()
            .set(false, "Cargo.toml", &cargo("0.1.0", "build = false\n[lib]\npath = \"src/lib.rs\"\n[lints.rust]\nunsafe_code = \"forbid\""))
            .set(true, "Cargo.toml", &cargo("0.2.0", "build = false\n[lib]\npath = \"src/lib.rs\"\n[lints.rust]\nunsafe_code = \"forbid\""));
        assert!(p.rules().is_empty(), "{:?}", p.rules());
    }

    #[test]
    fn hashes_are_checked_first() {
        let old = zip(&[Entry::file("a", b"")]);
        let new = zip(&[Entry::file("b", b"")]);
        let update = Update {
            id: "demo",
            version: "0.2.0",
            old: &old,
            old_sha256: &crate::sha256_hex(&new),
            new: &new,
            new_sha256: &crate::sha256_hex(&new),
            pins: &[],
        };
        assert_eq!(check(&update).rules(), [crate::Rule::Hash]);
    }

    #[test]
    fn a_changed_cargo_toml_is_refused() {
        let changed = |extra: &str| {
            Pair::new()
                .set(true, "Cargo.toml", &cargo("0.2.0", extra))
                .rules()
        };
        assert_eq!(changed("rust-version = \"1.90\""), ["R8"]);
        assert_eq!(
            changed("edition = \"2021\"").first().map(String::as_str),
            Some("R8")
        );
        let renamed = cargo("0.2.0", "").replace("name = \"demo\"", "name = \"other\"");
        assert_eq!(
            Pair::new().set(true, "Cargo.toml", &renamed).rules(),
            ["R8"]
        );
        assert_eq!(
            Pair::new().set(true, "Cargo.toml", "[package").rules(),
            ["R8"]
        );
    }

    #[test]
    fn build_inputs_beyond_the_sdk_are_refused_even_when_unchanged() {
        for extra in [
            "build = \"build.rs\"",
            "build = true",
            "links = \"z\"",
            "[lib]\nproc-macro = true",
            "[lib]\npath = \"assets/x.rs\"",
            "[build-dependencies]\ncc = \"1\"",
            "[dev-dependencies]\nrand = \"1\"",
            "[target.'cfg(unix)'.dependencies]\nlibc = \"1\"",
            "[patch.crates-io]\nserde = { git = \"https://example.com/serde\" }",
            "[features]\ndefault = []",
            "[profile.release]\nlto = true",
            "[workspace]\nmembers = [\"assets/x\"]",
            "[workspace]",
            "[[bin]]\nname = \"x\"\npath = \"src/x.rs\"",
        ] {
            let p = Pair::new()
                .set(false, "Cargo.toml", &cargo("0.1.0", extra))
                .set(true, "Cargo.toml", &cargo("0.2.0", extra));
            assert_eq!(p.rules(), ["R9"], "{extra}");
        }
        let deps = |deps: &str| {
            let text = |v: &str| {
                cargo(v, "").replace(
                    "horizon-mod-api = { path = \".horizon/sdk/api/crates/horizon-mod-api\" }",
                    deps,
                )
            };
            Pair::new()
                .set(false, "Cargo.toml", &text("0.1.0"))
                .set(true, "Cargo.toml", &text("0.2.0"))
                .rules()
        };
        assert_eq!(
            deps(
                "horizon-mod-api = { path = \".horizon/sdk/api/crates/horizon-mod-api\" }\nserde_json = \"1\""
            ),
            ["R9"]
        );
        assert_eq!(deps("horizon-mod-api = { path = \"src/engine\" }"), ["R9"]);
        assert_eq!(
            deps("horizon-mod-api = { git = \"https://example.com/x\" }"),
            ["R9"]
        );
        assert_eq!(
            deps(
                "horizon-mod-api = { path = \".horizon/sdk/api/crates/horizon-mod-api\", features = [\"x\"] }"
            ),
            ["R9"]
        );
        assert_eq!(deps("horizon-mod-api = \"0.2\""), ["R9"]);
        assert_eq!(deps(""), ["R9"]);
    }

    #[test]
    fn the_lock_may_change_only_the_mods_own_version() {
        let p = Pair::new().set(false, "Cargo.lock", &lock("0.1.0")).set(
            true,
            "Cargo.lock",
            &lock("0.2.0"),
        );
        assert!(p.rules().is_empty());
        let p = Pair::new().set(false, "Cargo.lock", &lock("0.1.0")).set(
            true,
            "Cargo.lock",
            &lock("0.1.0"),
        );
        assert!(p.rules().is_empty());
        // Dropped from the new package: the launcher resolves against its own SDK.
        assert!(
            Pair::new()
                .set(false, "Cargo.lock", &lock("0.1.0"))
                .rules()
                .is_empty()
        );
        assert_eq!(
            Pair::new().set(true, "Cargo.lock", &lock("0.2.0")).rules(),
            ["R10"]
        );
        let other = format!(
            "{}\n[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
            lock("0.2.0")
        );
        let p =
            Pair::new()
                .set(false, "Cargo.lock", &lock("0.1.0"))
                .set(true, "Cargo.lock", &other);
        assert_eq!(p.rules(), ["R10"]);
        let sdk = lock("0.2.0").replace(
            "name = \"horizon-mod-api\"\nversion = \"0.2.0\"",
            "name = \"horizon-mod-api\"\nversion = \"0.3.0\"",
        );
        let p = Pair::new()
            .set(false, "Cargo.lock", &lock("0.1.0"))
            .set(true, "Cargo.lock", &sdk);
        assert_eq!(p.rules(), ["R10"]);
    }

    #[test]
    fn native_tables_are_refused() {
        for extra in [
            "internals = [{ path = \"game::x\" }]",
            "[[inject]]\nid = \"a\"",
            "[[injection]]\nid = \"a\"",
            "[wasm]\nfile = \"mod.wasm\"",
        ] {
            let p = Pair::new()
                .set(false, "horizon-mod.toml", &manifest("0.1.0", H1, M1, extra))
                .set(true, "horizon-mod.toml", &manifest("0.2.0", H2, M2, extra));
            assert_eq!(p.rules(), ["R11"], "{extra}");
        }
        for table in [
            "[[native.mixin]]\nid = \"a\"",
            "[[native.module]]\nid = \"a\"",
            "[[native.bridge]]\nid = \"a\"",
            "[[native.state]]\nid = \"a\"",
            "[[native.loop]]\nid = \"a\"",
            "[[native.injection]]\nid = \"a\"",
        ] {
            let p = Pair::new()
                .set(
                    false,
                    "horizon-mod.toml",
                    &format!("{}{table}\n", manifest("0.1.0", H1, M1, "")),
                )
                .set(
                    true,
                    "horizon-mod.toml",
                    &format!("{}{table}\n", manifest("0.2.0", H2, M2, "")),
                );
            assert_eq!(p.rules(), ["R11"], "{table}");
        }
        let world =
            manifest("0.2.0", H2, M2, "").replace("[native]\n", "[native]\nworld_control = true\n");
        assert_eq!(
            Pair::new().set(true, "horizon-mod.toml", &world).rules(),
            ["R11", "R12"]
        );
    }

    #[test]
    fn a_changed_manifest_is_refused() {
        let changed = |extra: &str| {
            Pair::new()
                .set(true, "horizon-mod.toml", &manifest("0.2.0", H2, M2, extra))
                .rules()
        };
        assert_eq!(changed("homepage = \"https://example.com\""), ["R12"]);
        assert_eq!(changed("[dependencies]\nother = \"1\""), ["R12"]);
        let game = manifest("0.2.0", H2, M2, "").replace("26.3", "26.4");
        assert_eq!(
            Pair::new().set(true, "horizon-mod.toml", &game).rules(),
            ["R12"]
        );
        let id = manifest("0.2.0", H2, M2, "").replace("\"demo\"", "\"other\"");
        assert_eq!(
            Pair::new().set(true, "horizon-mod.toml", &id).rules(),
            ["R12"]
        );
        // The entry names 0.2.0.
        let version = manifest("0.3.0", H2, M2, "");
        assert_eq!(
            Pair::new().set(true, "horizon-mod.toml", &version).rules(),
            ["R12"]
        );
        assert_eq!(
            Pair::new().set(true, "horizon-mod.toml", "id = ").rules(),
            ["R12"]
        );
    }

    #[test]
    fn pins_must_be_released_and_mixins_the_engines() {
        let pin = |h: &str, m: &str| {
            Pair::new()
                .set(true, "horizon-mod.toml", &manifest("0.2.0", h, m, ""))
                .rules()
        };
        let fork = "cccccccccccccccccccccccccccccccccccccccc";
        assert_eq!(pin(fork, M2), ["R13"]);
        assert_eq!(pin(H2, fork), ["R13"]);
        assert_eq!(pin(H2, M1), ["R13"]);
        assert!(pin(H1, M1).is_empty());
    }
}
