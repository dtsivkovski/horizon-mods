//! Every `.rs` file of the new package, reachable or not, parsed with `syn`. Two passes:
//!
//! - the syntax tree: attributes, paths, `use` trees and macros, outside items under exactly
//!   `#[cfg(test)]`. Each allowed macro's arguments are parsed by that macro's own grammar
//!   (expressions, a pattern, `static` items, a literal, a `cfg` predicate) and checked as part of
//!   the tree; arguments that don't parse that way are refused, never scanned as tokens instead;
//! - as a backstop, the tokens of the whole file, the same ones `syn` parsed, outside those items:
//!   the `unsafe` and `extern` keywords and every macro name anywhere.
//!
//! Syntactic only: what a method call on a value the SDK returns does is not seen.

use proc_macro2::{Delimiter, LineColumn, Spacing, Span, TokenStream, TokenTree};
use syn::parse::{ParseStream, Parser};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ImplItem, Item, ItemUse, Meta, Pat, Path, Token, TraitItem, Type, UseTree,
};

use crate::package::Package;
use crate::report::{Report, Rule, Side};

pub const ROOTS: [&str; 3] = ["std", "core", "alloc"];

/// Data-only modules of `std`, `core` and `alloc`. Not `fs`, `net`, `process`, `env`, `os`, `path`,
/// `ffi`, `thread`, `io`, `ptr`, `arch`, `alloc`, `panic` or `prelude` (whose `v1` re-exports
/// `include!` and `env!` under their own names).
pub const MODULES: &[&str] = &[
    "any",
    "array",
    "ascii",
    "borrow",
    "boxed",
    "cell",
    "char",
    "clone",
    "cmp",
    "collections",
    "convert",
    "default",
    "error",
    "f32",
    "f64",
    "fmt",
    "hash",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "iter",
    "marker",
    "mem",
    "num",
    "ops",
    "option",
    "rc",
    "result",
    "slice",
    "str",
    "string",
    "sync",
    "time",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "vec",
    "primitive",
];

/// Macros by name, from `std`/`core`/`alloc` or the SDK (`json`). `include_str!` and
/// `include_bytes!` also pass [`Checker::include`].
pub const MACROS: &[&str] = &[
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "format",
    "format_args",
    "write",
    "writeln",
    "print",
    "println",
    "eprint",
    "eprintln",
    "vec",
    "matches",
    "todo",
    "unimplemented",
    "unreachable",
    "panic",
    "cfg",
    "json",
    "include_str",
    "include_bytes",
    "column",
    "line",
    "thread_local",
];

/// The first segment of a macro path with more than one: std's crates and the SDK.
const MACRO_ROOTS: [&str; 5] = ["std", "core", "alloc", "mod_api", "serde_json"];

/// Attributes by path. Lints, docs, derives, tests and `cfg`; the SDK's `horizon_mod` and
/// `horizon_part`; and hints that change no behaviour. Not `no_mangle`, `export_name`, `link*`,
/// `path`, `feature`, `global_allocator`, `used`, `macro_use`, `no_std`, ...
pub const ATTRIBUTES: &[&str] = &[
    "derive",
    // derive(Default)'s variant marker.
    "default",
    "test",
    "ignore",
    "should_panic",
    "cfg",
    "cfg_attr",
    "allow",
    "warn",
    "deny",
    "forbid",
    "expect",
    "doc",
    "must_use",
    "inline",
    "cold",
    "track_caller",
    "non_exhaustive",
    "deprecated",
    "repr",
    "rustfmt::skip",
    "horizon_mod",
    "horizon_part",
    "mod_api::horizon_mod",
    "mod_api::horizon_part",
];

/// Words that can come before `!` without naming a macro (`if !x`, `return !(a)`).
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "yield",
];

pub fn allowed_module(name: &str) -> bool {
    MODULES.contains(&name)
}

/// An identifier as the compiler resolves it: `r#std` is `std`.
fn unraw(ident: &proc_macro2::Ident) -> String {
    let s = ident.to_string();
    s.strip_prefix("r#").map(str::to_owned).unwrap_or(s)
}

fn path_names(path: &Path) -> Vec<String> {
    path.segments.iter().map(|s| unraw(&s.ident)).collect()
}

/// Exactly `#[cfg(test)]`: compiled only into the mod's own tests, never by the launcher.
fn is_cfg_test(attr: &Attribute) -> bool {
    let Meta::List(list) = &attr.meta else {
        return false;
    };
    if !matches!(attr.style, syn::AttrStyle::Outer) || !list.path.is_ident("cfg") {
        return false;
    }
    let tokens: Vec<TokenTree> = list.tokens.clone().into_iter().collect();
    matches!(tokens.as_slice(), [TokenTree::Ident(i)] if i == "test")
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

/// `forbid(unsafe_code)`, or a `cfg_attr` carrying one under any predicate, as the launcher reads it.
fn forbids_unsafe(meta: &Meta) -> bool {
    let Meta::List(list) = meta else {
        return false;
    };
    let Ok(items) = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated) else {
        return false;
    };
    if list.path.is_ident("forbid") {
        items.iter().any(|m| m.path().is_ident("unsafe_code"))
    } else if list.path.is_ident("cfg_attr") {
        items.iter().skip(1).any(forbids_unsafe)
    } else {
        false
    }
}

/// An allowed macro's arguments, parsed by its grammar.
enum Args {
    Exprs(Vec<Expr>),
    Matches(Box<(Expr, Pat, Option<Expr>)>),
    Predicate(Box<Meta>),
    Statics(Vec<(Vec<Attribute>, Type, Expr)>),
    Include(syn::LitStr),
    Nothing,
}

/// `format!`, `assert_eq!`, `write!`, `panic!` and the like: expressions, comma-separated (a named
/// format argument `x = y` is an assignment expression).
fn exprs(input: ParseStream) -> syn::Result<Vec<Expr>> {
    Ok(Punctuated::<Expr, Token![,]>::parse_terminated(input)?
        .into_iter()
        .collect())
}

/// `vec![a, b]` or `vec![value; count]`.
fn vec_args(input: ParseStream) -> syn::Result<Vec<Expr>> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let first: Expr = input.parse()?;
    if input.peek(Token![;]) {
        input.parse::<Token![;]>()?;
        return Ok(vec![first, input.parse()?]);
    }
    let mut out = vec![first];
    while !input.is_empty() {
        input.parse::<Token![,]>()?;
        if input.is_empty() {
            break;
        }
        out.push(input.parse()?);
    }
    Ok(out)
}

/// `matches!(expression, pattern if guard,)`.
fn matches_args(input: ParseStream) -> syn::Result<(Expr, Pat, Option<Expr>)> {
    let expr = input.parse()?;
    input.parse::<Token![,]>()?;
    let pat = Pat::parse_multi_with_leading_vert(input)?;
    let guard = if input.peek(Token![if]) {
        input.parse::<Token![if]>()?;
        Some(input.parse()?)
    } else {
        None
    };
    if input.peek(Token![,]) {
        input.parse::<Token![,]>()?;
    }
    Ok((expr, pat, guard))
}

/// `thread_local!`: `#[attr] pub static NAME: Type = init;`, any number.
fn statics(input: ParseStream) -> syn::Result<Vec<(Vec<Attribute>, Type, Expr)>> {
    let mut out = Vec::new();
    while !input.is_empty() {
        let attrs = input.call(Attribute::parse_outer)?;
        input.parse::<syn::Visibility>()?;
        input.parse::<Token![static]>()?;
        input.parse::<syn::Ident>()?;
        input.parse::<Token![:]>()?;
        let ty = input.parse()?;
        input.parse::<Token![=]>()?;
        out.push((attrs, ty, input.parse()?));
        if input.is_empty() {
            break;
        }
        input.parse::<Token![;]>()?;
    }
    Ok(out)
}

/// `json!`: an object `{ key: value, ... }`, an array `[value, ...]`, or an expression; keys and
/// expressions are collected for the tree pass.
fn json_value(input: ParseStream, out: &mut Vec<Expr>) -> syn::Result<()> {
    let list = |content: ParseStream, out: &mut Vec<Expr>, object: bool| -> syn::Result<()> {
        while !content.is_empty() {
            if object {
                out.push(content.parse()?);
                content.parse::<Token![:]>()?;
            }
            json_value(content, out)?;
            if content.is_empty() {
                break;
            }
            content.parse::<Token![,]>()?;
        }
        Ok(())
    };
    if input.peek(syn::token::Brace) {
        let content;
        syn::braced!(content in input);
        list(&content, out, true)
    } else if input.peek(syn::token::Bracket) {
        let content;
        syn::bracketed!(content in input);
        list(&content, out, false)
    } else {
        out.push(input.parse()?);
        Ok(())
    }
}

fn json_args(input: ParseStream) -> syn::Result<Vec<Expr>> {
    let mut out = Vec::new();
    json_value(input, &mut out)?;
    Ok(out)
}

pub fn check(report: &mut Report, package: &Package) {
    for (path, bytes) in package.texts.iter().filter(|(p, _)| p.ends_with(".rs")) {
        check_file(report, package, path, bytes);
    }
}

pub fn check_file(report: &mut Report, package: &Package, path: &str, bytes: &[u8]) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        report.add(Rule::Parse, Side::New, path, 0, "not UTF-8");
        return;
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // One lexing for both passes, so the spans of exempt items are the tokens' own.
    let tokens: TokenStream = match text.parse() {
        Ok(tokens) => tokens,
        Err(e) => {
            report.add(
                Rule::Parse,
                Side::New,
                path,
                e.span().start().line,
                format!("doesn't lex: {e}"),
            );
            return;
        }
    };
    let file: syn::File = match syn::parse2(tokens.clone()) {
        Ok(file) => file,
        Err(e) => {
            report.add(
                Rule::Parse,
                Side::New,
                path,
                e.span().start().line,
                format!("doesn't parse: {e}"),
            );
            return;
        }
    };
    let mut checker = Checker {
        report,
        package,
        path,
        exempt: Vec::new(),
    };
    // The launcher appends `#[unsafe(no_mangle)]` exports to the crate root and refuses a root that
    // forbids unsafe code (`prebuilt.rs`'s `check_entry_root`).
    if path == "src/lib.rs" {
        for attr in file.attrs.iter().filter(|a| forbids_unsafe(&a.meta)) {
            checker.add(
                Rule::Attribute,
                attr.span(),
                "the crate root forbids unsafe code, which the launcher refuses: deny it instead",
            );
        }
    }
    checker.visit_file(&file);
    checker.tokens(tokens);
}

struct Checker<'a> {
    report: &'a mut Report,
    package: &'a Package,
    path: &'a str,
    /// The spans of items under exactly `#[cfg(test)]`.
    exempt: Vec<(LineColumn, LineColumn)>,
}

impl Checker<'_> {
    fn add(&mut self, rule: Rule, span: Span, detail: impl Into<String>) {
        self.report
            .add(rule, Side::New, self.path, span.start().line, detail);
    }

    fn is_exempt(&self, span: Span) -> bool {
        let (start, end) = (span.start(), span.end());
        self.exempt.iter().any(|(s, e)| {
            (s.line, s.column) <= (start.line, start.column)
                && (end.line, end.column) <= (e.line, e.column)
        })
    }

    /// `std`, `core` and `alloc` anywhere in a path are followed by an allowed module.
    fn segments(&mut self, names: &[String], span: Span) {
        for pair in names.windows(2) {
            if ROOTS.contains(&pair[0].as_str()) && !allowed_module(&pair[1]) {
                self.add(
                    Rule::StdModule,
                    span,
                    format!("{}::{} is not allowed", pair[0], pair[1]),
                );
            }
        }
    }

    fn attribute(&mut self, attr: &Attribute) {
        self.meta(&attr.meta);
    }

    fn meta(&mut self, meta: &Meta) {
        let name = path_names(meta.path()).join("::");
        if !ATTRIBUTES.contains(&name.as_str()) {
            self.add(
                Rule::Attribute,
                meta.span(),
                format!("#[{name}] is not allowed"),
            );
            return;
        }
        match meta {
            Meta::Path(_) => {}
            // `#[doc = include_str!(..)]`: the macro is checked like any other.
            Meta::NameValue(nv)
                if name == "doc" && matches!(nv.value, Expr::Lit(_) | Expr::Macro(_)) =>
            {
                self.visit_expr(&nv.value)
            }
            Meta::NameValue(nv) => self.plain_meta(meta, nv.span(), Rule::Attribute),
            Meta::List(list) if name.starts_with("horizon_") || name.starts_with("mod_api::") => {
                self.add(
                    Rule::Attribute,
                    list.span(),
                    format!("#[{name}] takes no arguments"),
                )
            }
            Meta::List(list) => match name.as_str() {
                "cfg_attr" => {
                    let parser = syn::punctuated::Punctuated::<Meta, Token![,]>::parse_terminated;
                    match list.parse_args_with(parser) {
                        // The first is the predicate; the rest are attributes like any other.
                        Ok(metas) => metas.iter().enumerate().for_each(|(i, m)| {
                            if i == 0 {
                                self.plain_meta(m, m.span(), Rule::Attribute)
                            } else {
                                self.meta(m)
                            }
                        }),
                        Err(_) => {
                            self.add(Rule::Attribute, list.span(), "#[cfg_attr] doesn't parse")
                        }
                    }
                }
                "repr" => {
                    match list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated) {
                        Ok(items) => items.iter().for_each(|m| self.repr(m)),
                        Err(_) => self.add(Rule::Attribute, list.span(), "#[repr] doesn't parse"),
                    }
                }
                "derive" => {
                    let parser = syn::punctuated::Punctuated::<Path, Token![,]>::parse_terminated;
                    match list.parse_args_with(parser) {
                        Ok(paths) => paths.iter().for_each(|p| self.visit_path(p)),
                        Err(_) => self.add(Rule::Attribute, list.span(), "#[derive] doesn't parse"),
                    }
                }
                _ => self.plain_meta(meta, list.span(), Rule::Attribute),
            },
        }
    }

    /// One item of `#[repr(..)]`: a representation, or `align(N)` / `packed(N)` with an integer.
    fn repr(&mut self, meta: &Meta) {
        const REPRS: &[&str] = &[
            "C",
            "Rust",
            "transparent",
            "packed",
            "u8",
            "u16",
            "u32",
            "u64",
            "u128",
            "usize",
            "i8",
            "i16",
            "i32",
            "i64",
            "i128",
            "isize",
        ];
        let ok = match meta {
            Meta::Path(p) => p
                .get_ident()
                .is_some_and(|i| REPRS.contains(&i.to_string().as_str())),
            Meta::List(list) => {
                (list.path.is_ident("align") || list.path.is_ident("packed"))
                    && syn::parse2::<syn::LitInt>(list.tokens.clone()).is_ok()
            }
            Meta::NameValue(_) => false,
        };
        if !ok {
            self.add(
                Rule::Attribute,
                meta.span(),
                "#[repr] takes a representation, align(N) or packed(N)",
            );
        }
    }

    /// Arguments that hold no code: paths, literals, and lists of those (lints, `cfg` predicates,
    /// `deprecated(note = ..)`).
    fn plain_meta(&mut self, meta: &Meta, span: Span, rule: Rule) {
        match meta {
            Meta::Path(_) => {}
            Meta::NameValue(nv) if matches!(nv.value, Expr::Lit(_)) => {}
            Meta::NameValue(_) => self.add(rule, span, "an argument value other than a literal"),
            Meta::List(list) => {
                match list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated) {
                    Ok(items) => items.iter().for_each(|m| self.plain_meta(m, span, rule)),
                    Err(_) => self.add(rule, span, "arguments that aren't paths or literals"),
                }
            }
        }
    }

    fn use_tree(&mut self, prefix: &mut Vec<String>, tree: &UseTree) {
        let full = |prefix: &Vec<String>, ident: &proc_macro2::Ident| {
            let mut full = prefix.clone();
            if ident != "self" {
                full.push(unraw(ident));
            }
            full
        };
        match tree {
            UseTree::Path(p) => {
                prefix.push(unraw(&p.ident));
                self.use_tree(prefix, &p.tree);
                prefix.pop();
            }
            UseTree::Name(n) => self.segments(&full(prefix, &n.ident), n.ident.span()),
            UseTree::Rename(r) => {
                let names = full(prefix, &r.ident);
                self.segments(&names, r.ident.span());
                if names.last().is_some_and(|n| ROOTS.contains(&n.as_str())) {
                    self.add(
                        Rule::StdRoot,
                        r.ident.span(),
                        format!("{} renamed", names.join("::")),
                    );
                }
                // An allowed macro's name for something else.
                if MACROS.contains(&unraw(&r.rename).as_str()) {
                    self.add(
                        Rule::Macro,
                        r.rename.span(),
                        format!("renamed to {}", unraw(&r.rename)),
                    );
                }
            }
            UseTree::Glob(g) => {
                self.segments(prefix, g.star_token.span);
                if prefix.last().is_none_or(|n| ROOTS.contains(&n.as_str())) {
                    self.add(
                        Rule::StdRoot,
                        g.star_token.span,
                        format!("{}::* glob", prefix.join("::")),
                    );
                }
            }
            UseTree::Group(g) => g.items.iter().for_each(|t| self.use_tree(prefix, t)),
        }
    }

    fn macro_path(&mut self, path: &Path) {
        let names = path_names(path);
        let last = names.last().map(String::as_str).unwrap_or("");
        // The token pass refuses it as R17.
        if names == ["macro_rules"] {
            return;
        }
        let rooted = names.len() == 1 || MACRO_ROOTS.contains(&names[0].as_str());
        if !MACROS.contains(&last) || !rooted {
            self.add(
                Rule::Macro,
                path.span(),
                format!("{}! is not allowed", names.join("::")),
            );
        }
    }

    /// An allowed macro's arguments, by its grammar, through the tree pass.
    fn macro_args(&mut self, mac: &syn::Macro) {
        let names = path_names(&mac.path);
        let Some(name) = names.last().filter(|n| MACROS.contains(&n.as_str())) else {
            return;
        };
        let tokens = mac.tokens.clone();
        let parsed = match name.as_str() {
            "vec" => vec_args.parse2(tokens).map(Args::Exprs),
            "matches" => matches_args
                .parse2(tokens)
                .map(|m| Args::Matches(Box::new(m))),
            "cfg" => syn::parse2::<Meta>(tokens).map(|m| Args::Predicate(Box::new(m))),
            "json" => json_args.parse2(tokens).map(Args::Exprs),
            "thread_local" => statics.parse2(tokens).map(Args::Statics),
            "line" | "column" if tokens.is_empty() => Ok(Args::Nothing),
            "line" | "column" => Err(syn::Error::new(mac.span(), "takes no arguments")),
            "include_str" | "include_bytes" => match syn::parse2::<syn::LitStr>(tokens) {
                Ok(lit) => Ok(Args::Include(lit)),
                Err(_) => {
                    return self.add(
                        Rule::Include,
                        mac.span(),
                        "argument is not one string literal",
                    );
                }
            },
            _ => exprs.parse2(tokens).map(Args::Exprs),
        };
        match parsed {
            Err(e) => self.add(
                Rule::Macro,
                mac.span(),
                format!("{name}!'s arguments don't parse as its grammar: {e}"),
            ),
            Ok(Args::Exprs(exprs)) => exprs.iter().for_each(|e| self.visit_expr(e)),
            Ok(Args::Matches(m)) => {
                let (expr, pat, guard) = *m;
                self.visit_expr(&expr);
                self.visit_pat(&pat);
                guard.iter().for_each(|g| self.visit_expr(g));
            }
            Ok(Args::Predicate(meta)) => self.plain_meta(&meta, mac.span(), Rule::Macro),
            Ok(Args::Statics(statics)) => {
                for (attrs, ty, init) in &statics {
                    attrs.iter().for_each(|a| self.attribute(a));
                    self.visit_type(ty);
                    self.visit_expr(init);
                }
            }
            Ok(Args::Include(lit)) => self.include(&lit),
            Ok(Args::Nothing) => {}
        }
    }

    /// `include_str!`/`include_bytes!`: a relative path from this file's directory that stays in
    /// the package once `.` and `..` are resolved, naming a non-Rust file under `assets/` or
    /// `data/` (regular files only: the package has no links).
    fn include(&mut self, lit: &syn::LitStr) {
        let value = lit.value();
        let mut target: Vec<&str> = self.path.split('/').collect();
        target.pop();
        let mut bad =
            value.is_empty() || value.starts_with('/') || value.contains(['\\', ':', '\0']);
        for part in value.split('/') {
            match part {
                "." => {}
                ".." => bad |= target.pop().is_none(),
                "" => bad = true,
                _ => target.push(part),
            }
        }
        let target = target.join("/");
        let data = target.starts_with("assets/") || target.starts_with("data/");
        if bad
            || !data
            || target.to_ascii_lowercase().ends_with(".rs")
            || !self.package.files.contains(&target)
        {
            self.add(
                Rule::Include,
                lit.span(),
                format!("{value:?} is not a non-Rust file of the package's assets/ or data/"),
            );
        }
    }

    /// The token pass over the whole file, outside exempt items.
    fn tokens(&mut self, stream: TokenStream) {
        let tokens: Vec<TokenTree> = stream.into_iter().collect();
        for (i, token) in tokens.iter().enumerate() {
            if self.is_exempt(token.span()) {
                continue;
            }
            match token {
                TokenTree::Group(g) => self.tokens(g.stream()),
                TokenTree::Ident(id) => {
                    let name = unraw(id);
                    match id.to_string().as_str() {
                        "unsafe" => self.add(Rule::Unsafe, id.span(), "unsafe"),
                        "extern" => self.add(Rule::Extern, id.span(), "extern"),
                        _ => {}
                    }
                    let bang = matches!(tokens.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == '!' && p.spacing() == Spacing::Alone);
                    if !bang || KEYWORDS.contains(&name.as_str()) {
                        continue;
                    }
                    match tokens.get(i + 2) {
                        _ if name == "macro_rules" => {
                            self.add(Rule::MacroRules, id.span(), "macro_rules!")
                        }
                        Some(TokenTree::Group(g))
                            if g.delimiter() != Delimiter::None
                                && !MACROS.contains(&name.as_str()) =>
                        {
                            self.add(Rule::Macro, id.span(), format!("{name}! is not allowed"));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Checker<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        let attrs = item_attrs(item);
        if attrs.iter().any(is_cfg_test) {
            let span = item.span();
            self.exempt.push((span.start(), span.end()));
            attrs.iter().for_each(|a| self.attribute(a));
            return;
        }
        match item {
            Item::Verbatim(t) => {
                self.add(Rule::Parse, t.span(), "item syntax the checker can't read")
            }
            Item::ForeignMod(i) => self.add(Rule::Extern, i.span(), "extern block"),
            Item::ExternCrate(i) => self.add(Rule::Extern, i.span(), "extern crate"),
            _ => {}
        }
        visit::visit_item(self, item);
    }

    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        self.attribute(attr);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.macro_path(&mac.path);
        self.macro_args(mac);
    }

    fn visit_path(&mut self, path: &'ast Path) {
        self.segments(&path_names(path), path.span());
        visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        item.attrs.iter().for_each(|a| self.attribute(a));
        self.use_tree(&mut Vec::new(), &item.tree);
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let Expr::Verbatim(t) = expr {
            self.add(
                Rule::Parse,
                t.span(),
                "expression syntax the checker can't read",
            );
        }
        visit::visit_expr(self, expr);
    }

    fn visit_type(&mut self, ty: &'ast Type) {
        if let Type::Verbatim(t) = ty {
            self.add(Rule::Parse, t.span(), "type syntax the checker can't read");
        }
        visit::visit_type(self, ty);
    }

    fn visit_pat(&mut self, pat: &'ast Pat) {
        if let Pat::Verbatim(t) = pat {
            self.add(
                Rule::Parse,
                t.span(),
                "pattern syntax the checker can't read",
            );
        }
        visit::visit_pat(self, pat);
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        if let ImplItem::Verbatim(t) = item {
            self.add(
                Rule::Parse,
                t.span(),
                "impl item syntax the checker can't read",
            );
        }
        visit::visit_impl_item(self, item);
    }

    fn visit_trait_item(&mut self, item: &'ast TraitItem) {
        if let TraitItem::Verbatim(t) = item {
            self.add(
                Rule::Parse,
                t.span(),
                "trait item syntax the checker can't read",
            );
        }
        visit::visit_trait_item(self, item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule ids `files` break, each file a `(path, source)`.
    fn rules_of(files: &[(&str, &str)]) -> Vec<String> {
        let mut package = Package::default();
        for (path, text) in files {
            package.files.insert((*path).to_owned());
            package
                .texts
                .insert((*path).to_owned(), text.as_bytes().to_vec());
        }
        for file in [
            "assets/x/a.json",
            "data/x/d.json",
            "assets/x/y.rs",
            "README.md",
        ] {
            package.files.insert(file.into());
        }
        let mut report = Report::default();
        check(&mut report, &package);
        report.rules().iter().map(|r| r.id()).collect()
    }

    fn rules(src: &str) -> Vec<String> {
        rules_of(&[("src/lib.rs", src)])
    }

    #[track_caller]
    fn refused(src: &str, want: &[&str]) {
        assert_eq!(rules(src), want, "{src}");
    }

    #[test]
    fn plain_safe_rust_passes() {
        let src = r#"
            //! Mentions of unsafe, extern and std::fs in docs and strings don't count.
            #![allow(dead_code)]
            #![cfg_attr(test, allow(unused))]
            use std::collections::{BTreeMap, HashMap};
            use std::sync::{Arc, Mutex, atomic::{AtomicU32, Ordering}};
            use std::fmt::Write as _;
            use core::cmp::Ordering as Order;
            use std::time::{Duration, Instant};
            use std;
            use mod_api::serde_json::{self, json};
            use mod_api::{Hooks, ServerHost};
            pub mod runtime;

            /// Not `unsafe { std::fs::read("x") }`.
            #[derive(Debug, Default, Clone, PartialEq)]
            #[must_use]
            pub struct State { core: Mutex<u32>, std: u8 }

            #[derive(Default)]
            enum Mode { #[default] A, B }

            thread_local! { static CACHE: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) }; }

            #[mod_api::horizon_mod]
            impl Hooks for State {
                #[inline]
                fn tick(&self) {
                    let s = format!("{} {}", "unsafe", self.std);
                    let v = vec![1u8, 2, 3];
                    let j = json!({"a": 1});
                    let j2 = mod_api::serde_json::json!([1]);
                    let mut out = String::new();
                    let _ = write!(out, "{}", std::string::String::from("x"));
                    let _ = writeln!(out);
                    assert!(matches!(Some(1), Some(_)), "{}", self.core.lock().map(|g| *g).unwrap_or(0));
                    if !(v.len() != 3) { return; }
                    let n = !v.is_empty();
                    let d = std::time::Duration::from_secs(1) + Duration::ZERO;
                    let x = std::u32::MAX.min(core::f32::consts::PI as u32);
                    let a = include_str!("../assets/x/a.json");
                    let b = include_bytes!("./../data/x/d.json");
                    println!("{s}{j}{j2}{n}{d:?}{x}{a}{}", b.len());
                    let _: HashMap<u8, u8> = HashMap::new();
                    let _ = core::mem::size_of::<u8>() + core::mem::take(&mut 1usize);
                }
            }

            #[cfg(test)]
            mod tests {
                use std::fs;
                use std as s;
                extern crate alloc;
                macro_rules! m { () => {} }
                #[test]
                fn t() { unsafe { s::process::exit(0) }; let _ = env!("HOME"); fs::read("x"); }
            }
        "#;
        assert!(rules(src).is_empty(), "{:?}", rules(src));
    }

    #[test]
    fn unsafe_is_refused_everywhere() {
        refused("fn f() { unsafe { } }", &["R15"]);
        refused("unsafe fn f() {}", &["R15"]);
        refused("struct S; unsafe impl Send for S {}", &["R15"]);
        refused("unsafe trait T {}", &["R15"]);
        refused("type F = unsafe fn();", &["R15"]);
        refused("struct S; impl S { unsafe fn f(&self) {} }", &["R15"]);
        // Inside macro arguments the tree doesn't parse.
        refused("fn f() { let v = vec![unsafe { 1 }]; }", &["R15"]);
        refused(
            "fn f(p: *const u8) { println!(\"{}\", unsafe { *p }); }",
            &["R15"],
        );
        refused("fn f() { assert!({ { unsafe { true } } }); }", &["R15"]);
    }

    #[test]
    fn extern_is_refused() {
        refused("extern \"C\" fn f() {}", &["R16"]);
        refused("extern \"C\" { fn abort(); }", &["R16"]);
        refused("extern crate alloc;", &["R16"]);
        refused("type F = extern \"C\" fn();", &["R16"]);
        refused("fn f() { vec![{ extern \"C\" fn g() {} 1 }]; }", &["R16"]);
    }

    #[test]
    fn macro_rules_is_refused() {
        refused("macro_rules! m { () => {} }", &["R17"]);
        refused("fn f() { macro_rules! m { () => {} } }", &["R17"]);
        refused(
            "fn f() { vec![{ macro_rules! m { () => {} } 1 }]; }",
            &["R17"],
        );
    }

    #[test]
    fn attributes_outside_the_allowlist_are_refused() {
        for attr in [
            "#[no_mangle]",
            "#[export_name = \"x\"]",
            "#[link(name = \"c\")]",
            "#[link_name = \"x\"]",
            "#[link_section = \".x\"]",
            "#[used]",
            "#[cfg_attr(all(), no_mangle)]",
            "#[cfg_attr(test, allow(x), export_name = \"y\")]",
            "#[cfg_attr(all(), cfg_attr(all(), no_mangle))]",
            "#[rustfmt::other]",
            "#[horizon_loop]",
            "#[some::horizon_mod]",
        ] {
            refused(&format!("{attr} pub fn f() {{}}"), &["R18"]);
        }
        refused("#[path = \"/etc/passwd\"] mod x;", &["R18"]);
        refused("#[global_allocator] static A: u8 = 0;", &["R18"]);
        refused("#[macro_use] mod m {}", &["R18"]);
        refused("#![feature(asm)]", &["R18"]);
        refused("#![no_std]", &["R18"]);
        refused("#![no_implicit_prelude]", &["R18"]);
        refused("#[unsafe(no_mangle)] pub fn f() {}", &["R15", "R18"]);
        // In macro or SDK attribute arguments, std paths too.
        refused("#[mod_api::horizon_mod(std::fs::read)] struct S;", &["R18"]);
        refused("#[allow(x = { std::fs::read(\"x\") })] fn f() {}", &["R18"]);
        refused("#[cfg(any(unix, a = b))] fn f() {}", &["R18"]);
        refused(
            "#[deprecated(note = concat!(\"a\"))] fn f() {}",
            &["R18", "R19"],
        );
        refused("#[must_use = env!(\"HOME\")] fn f() {}", &["R18", "R19"]);
        refused("#[doc = env!(\"HOME\")] fn f() {}", &["R19"]);
        assert!(rules("#[allow(dead_code, clippy::x, reason = \"y\")] #[cfg(all(unix, not(test), feature = \"f\"))] #[deprecated(note = \"n\")] #[repr(u8)] enum E { A }").is_empty());
        refused("#[derive(std::io::Read)] struct S;", &["R21"]);
    }

    #[test]
    fn macros_outside_the_allowlist_are_refused() {
        for m in [
            "include!(\"x.rs\")",
            "env!(\"HOME\")",
            "option_env!(\"HOME\")",
            "concat!(\"a\", \"b\")",
            "stringify!(x)",
            "std::arch::asm!(\"nop\")",
            "core::arch::asm!(\"nop\")",
            "asm!(\"nop\")",
            "concat_idents!(a, b)",
            "log::warn!(\"x\")",
            "other::json!(\"x\")",
            "my_macro!(x)",
            "r#include!(\"x.rs\")",
        ] {
            refused(&format!("fn f() {{ let _ = {m}; }}"), &["R19"]);
        }
        refused("global_asm!(\"nop\");", &["R19"]);
        // Nested in the arguments of an allowed one.
        refused("fn f() { println!(\"{}\", env!(\"HOME\")); }", &["R19"]);
        refused(
            "fn f() { vec![format!(\"{}\", include!(\"x.rs\"))]; }",
            &["R19"],
        );
        // An allowed name given to something else.
        refused("use mod_api::other as format;", &["R19"]);
        refused("use std::include_str as format;", &["R19", "R21"]);
        refused("use std::prelude::v1::include_str as vec;", &["R19", "R21"]);
        refused("use std::prelude::v1::include;", &["R21"]);
    }

    #[test]
    fn include_reads_only_assets_and_data_of_the_package() {
        assert!(rules("const A: &str = include_str!(\"../assets/x/a.json\");").is_empty());
        assert!(rules("const A: &[u8] = include_bytes!(r\"../data/x/d.json\");").is_empty());
        assert!(rules("const A: &str = include_str!(\"../assets/../data/x/d.json\");").is_empty());
        // Relative to the file that includes it.
        let nested = [(
            "src/a/b.rs",
            "const A: &str = include_str!(\"../../assets/x/a.json\");",
        )];
        assert!(rules_of(&nested).is_empty());
        for arg in [
            "\"../../assets/x/a.json\"",
            "\"../../../etc/passwd\"",
            "\"/etc/passwd\"",
            "\"C:\\\\x\"",
            "\"lib.rs\"",
            "\"../assets/x/y.rs\"",
            "\"../README.md\"",
            "\"../assets/x/missing.json\"",
            "\"..//assets/x/a.json\"",
            "\"\"",
            "\"../assets/x/a.json\", ",
            "b\"../assets/x/a.json\"",
        ] {
            refused(&format!("const A: &str = include_str!({arg});"), &["R20"]);
        }
        refused(
            "const A: &str = include_str!(concat!(\"/etc/\", \"passwd\"));",
            &["R19", "R20"],
        );
        refused(
            "const A: &str = include_str!(env!(\"HOME\"));",
            &["R19", "R20"],
        );
        refused(
            "fn f() { println!(\"{}\", include_str!(\"/etc/passwd\")); }",
            &["R20"],
        );
        refused(
            "#[doc = include_str!(\"../README.md\")] fn f() {}",
            &["R20"],
        );
    }

    #[test]
    fn code_hidden_in_allowed_macro_arguments_is_checked_as_code() {
        refused(
            "fn f() { format!(\"{}\", { #[no_mangle] fn g() {} 0 }); }",
            &["R18"],
        );
        refused(
            "fn f() { format!(\"{}\", { #[path = \"x\"] mod m; 0 }); }",
            &["R18"],
        );
        refused("fn f() { format!(\"{}\", { use std::fs; 0 }); }", &["R21"]);
        refused("fn f() { assert!({ use std as s; true }); }", &["R22"]);
        refused("fn f() { println!(\"{}\", { unsafe { 1 } }); }", &["R15"]);
        refused("fn f() { vec![{ extern \"C\" fn g() {} 1 }]; }", &["R16"]);
        refused(
            "fn f() { assert_eq!(1, { mod m { include!(\"x.rs\"); } 1 }); }",
            &["R19"],
        );
        refused(
            "fn f() { let _ = matches!(1, x if { std::process::exit(0); x > 0 }); }",
            &["R21"],
        );
        refused(
            "fn f() { let _ = mod_api::serde_json::json!({ \"a\": [std::env::var(\"X\")] }); }",
            &["R21"],
        );
        refused("thread_local! { #[no_mangle] static A: u8 = 0; }", &["R18"]);
        refused(
            "thread_local! { static A: std::fs::File = todo!(); }",
            &["R21"],
        );
        refused(
            "fn f() { vec![std::net::TcpStream::connect(\"x\")]; }",
            &["R21"],
        );
    }

    #[test]
    fn macro_arguments_must_parse_by_the_macros_grammar() {
        for m in [
            "vec![1; 2; 3]",
            "vec![struct S;]",
            "assert_eq!(a b)",
            "format!(\"{}\", fn g() {})",
            "matches!(x)",
            "matches!(x, Some(_) if)",
            "line!(x)",
            "cfg!(a = { 1 })",
            "json!({ \"a\" 1 })",
            "json!({ \"a\": })",
            "json!([1 2])",
        ] {
            refused(&format!("fn f() {{ let _ = {m}; }}"), &["R19"]);
        }
        refused("thread_local! { fn f() {} }", &["R19"]);
        refused("thread_local! { static A: u8; }", &["R19"]);
    }

    #[test]
    fn allowed_macros_nest_and_qualify() {
        let src = r#"
            fn f(x: Option<u8>, v: Vec<u8>) {
                assert_eq!(std::format!("x"), "x");
                assert!(core::matches!(x, Some(1) | None if v.is_empty()), "{}", std::format!("{:?}", v));
                let _ = vec![0u8; 4];
                let _ = vec![vec![1], std::vec![2,]];
                let _ = mod_api::serde_json::json!({ "a": [1, v.len(), null], "b": { "c": true }, "d": x.map(|y| y + 1) });
                let _ = cfg!(any(unix, target_os = "windows"));
                debug_assert_ne!(line!(), column!());
                panic!("{n}", n = std::string::String::new());
            }
            thread_local! {
                pub static A: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
                static B: u8 = 1
            }
        "#;
        assert!(rules(src).is_empty(), "{:?}", rules(src));
    }

    #[test]
    fn repr_takes_representations_and_integer_alignments() {
        for attr in [
            "#[repr(C, align(8))]",
            "#[repr(C, packed(2))]",
            "#[repr(C, packed)]",
            "#[repr(transparent)]",
            "#[cfg_attr(unix, repr(C, align(16)))]",
        ] {
            assert!(rules(&format!("{attr} struct S(u8);")).is_empty(), "{attr}");
        }
        for attr in [
            "#[repr(align(N))]",
            "#[repr(align(8, 9))]",
            "#[repr(align = 8)]",
            "#[repr(packed(\"2\"))]",
            "#[repr(C, linkage(8))]",
            "#[repr(std::C)]",
            "#[repr(C, align(include_str!(\"x\")))]",
        ] {
            refused(&format!("{attr} struct S(u8);"), &["R18"]);
        }
    }

    #[test]
    fn a_crate_root_that_forbids_unsafe_code_is_refused_as_the_launcher_does() {
        refused("#![forbid(unsafe_code)]", &["R18"]);
        refused("#![forbid(missing_docs, unsafe_code)]", &["R18"]);
        refused("#![cfg_attr(not(test), forbid(unsafe_code))]", &["R18"]);
        assert!(rules("#![deny(unsafe_code)] #![forbid(missing_docs)]").is_empty());
        // Only the crate root.
        assert!(
            rules_of(&[("src/lib.rs", ""), ("src/m.rs", "#![forbid(unsafe_code)]")]).is_empty()
        );
    }

    #[test]
    fn std_modules_outside_the_allowlist_are_refused() {
        for src in [
            "use std::fs;",
            "use std::fs::read;",
            "use std::{collections::HashMap, fs};",
            "use std::{io::{self, Read}};",
            "use std::process::Command;",
            "use ::std::net::TcpStream;",
            "use std::env as e;",
            "use std::{fs as f};",
            "use core::ptr;",
            "use alloc::alloc::alloc;",
            "use std::os::unix::fs::PermissionsExt;",
            "use std::path::PathBuf;",
            "use std::ffi::CString;",
            "use std::thread;",
            "use std::panic;",
            "use std::prelude::v1::*;",
            "fn f() { std::fs::read(\"x\"); }",
            "fn f() { ::std::fs::read(\"x\"); }",
            "fn f() { r#std::fs::read(\"x\"); }",
            "fn f() { let _ = std::env::var(\"HOME\"); }",
            "fn f() -> std::io::Result<()> { Ok(()) }",
            "struct S; impl std::io::Write for S {}",
            "fn f<T: std::io::Read>(t: T) {}",
            "fn f() { <std::fs::File as Default>::default(); }",
            "use std; fn f() { crate::std::fs::read(\"x\"); }",
            "mod a { pub use std; } fn f() { a::std::process::exit(0); }",
            // Inside macro arguments.
            "fn f() { println!(\"{:?}\", std::fs::read(\"x\")); }",
            "fn f() { vec![::std::net::TcpStream::connect(\"x\")]; }",
            "fn f() { format!(\"{:?}\", { use std::{fs}; 1 }); }",
        ] {
            refused(src, &["R21"]);
        }
    }

    #[test]
    fn std_renames_and_globs_are_refused() {
        refused("use std as s; fn f() { s::fs::read(\"x\"); }", &["R22"]);
        refused("use ::core as c;", &["R22"]);
        refused("use std::{self as s};", &["R22"]);
        refused("mod a { pub use std; } use a::std as s;", &["R22"]);
        refused("use std::*;", &["R22"]);
        refused("use core::*;", &["R22"]);
        refused("use alloc::{*};", &["R22"]);
        refused("fn f() { vec![{ use std as s; 1 }]; }", &["R22"]);
        refused("extern crate std as s;", &["R16"]);
        // Globs of allowed modules are fine.
        assert!(rules("use std::collections::*; use self::inner::*; mod inner {}").is_empty());
    }

    #[test]
    fn only_exactly_cfg_test_is_exempt() {
        assert!(rules("#[cfg(test)] mod t { use std::fs; fn f() { unsafe {} } }").is_empty());
        assert!(rules("#[cfg(test)] fn f() { std::process::exit(0) }").is_empty());
        assert!(rules("mod a { #[cfg(test)] mod t { use std::fs; } }").is_empty());
        refused(
            "#[cfg(any(test, not(test)))] mod t { use std::fs; }",
            &["R21"],
        );
        refused("#[cfg(all(test))] mod t { fn f() { unsafe {} } }", &["R15"]);
        refused("#[cfg(not(test))] mod t { use std::fs; }", &["R21"]);
        refused(
            "#[cfg_attr(test, cfg(test))] mod t { use std::fs; }",
            &["R21"],
        );
        refused("#[cfg(test)] #[path = \"/etc/passwd\"] mod t;", &["R18"]);
        // An impl's items are not items: only whole items are exempt.
        refused(
            "struct S; impl S { #[cfg(test)] fn f() { std::process::exit(0) } }",
            &["R21"],
        );
        // A neighbour of an exempt item is still checked.
        refused("#[cfg(test)] mod t {} fn f() { unsafe {} }", &["R15"]);
    }

    #[test]
    fn every_rust_file_counts_reachable_or_not() {
        let files = [
            ("src/lib.rs", "pub fn a() {}"),
            ("src/orphan.rs", "fn f() { std::fs::read(\"x\"); }"),
        ];
        assert_eq!(rules_of(&files), ["R21"]);
        let files = [
            ("src/lib.rs", "pub fn a() {}"),
            ("assets/x/evil.rs", "unsafe fn f() {}"),
        ];
        assert_eq!(rules_of(&files), ["R15"]);
    }

    #[test]
    fn unparsable_files_are_refused() {
        refused("fn f( {", &["R14"]);
        refused("fn f() { \"unterminated }", &["R14"]);
        let mut package = Package::default();
        package.texts.insert("src/lib.rs".into(), vec![0xff, 0xfe]);
        let mut report = Report::default();
        check(&mut report, &package);
        assert_eq!(report.rules(), [Rule::Parse]);
    }

    #[test]
    fn negation_and_inequality_are_not_macros() {
        assert!(rules("fn f(a: bool, b: u8) -> bool { if !(a) { return !(a); } while !a {} match !(a) { _ => b != (1) } }").is_empty());
    }
}
