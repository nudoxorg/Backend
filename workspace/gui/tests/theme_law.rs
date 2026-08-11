//! The theme law — the palette restructure's own invariants, enforced instead
//! of trusted.
//!
//! # Why this file exists
//!
//! `src/theme/spec.rs`'s module doc makes a specific promise: "adding a theme
//! is one JSON file, one line in `BUNDLED`. No view changes, no role
//! assignment changes, no colour code anywhere." That promise has two halves,
//! and each half fails a different way if it stops being true.
//!
//! The first half — no colour code in a view — used to be silently violated.
//! Before the restructure a sweep of `workspace/gui/src` found colour literals
//! almost entirely absent, and **fifty-one** numeric `.opacity(N)` call sites
//! spanning sixteen distinct values, because the design system named every
//! colour and no transparency, so a view that needed "the same colour,
//! quieter" had nowhere to get the number from and invented one
//! (`src/theme/tokens.rs`'s `AlphaTokens` doc comment tells the whole story).
//! `no_view_contains_a_colour_literal` and `no_view_invents_a_transparency`
//! are what would have caught that the day it started, rather than at the
//! fifty-first site.
//!
//! The second half — no role assignment changes per theme — is the harder
//! promise, because a role assignment that quietly reaches past the palette
//! for a hand-picked `Hsla` is invisible in review: the type is the same, the
//! field is the same, only the *source* of the number differs.
//! `every_resolved_role_colour_comes_from_the_palette` is the load-bearing
//! test in this file for exactly that reason — it does not ask whether a
//! colour looks reasonable, it asks whether the palette could have produced
//! it at all.
//!
//! This is an integration test, so it goes through `lindsey::theme::*`, the
//! same surface a view or a second crate would use — never `crate::`.

use std::collections::HashSet;
use std::path::PathBuf;

use lindsey::theme::{
    AlphaTokens, ColourRoles, KindColours, NudoxThemeExt, Palette, SyntaxColours, ThemeLoadError,
    TrustStyle, TrustTokens, default_theme, parse_bundled, resolve, theme_by_key,
};

// ─────────────────────────────────────────────────────────────────────────────
// Shared source-scanning machinery
//
// Structure, comment-stripping approach and failure-message style copied from
// `tests/dependency_law.rs`, extended to also strip string literals — this
// file's patterns (`hsla(`, `Hsla {`, …) are words a doc example or an error
// message can legitimately contain, in a way `nudox_ir` mostly cannot.
// ─────────────────────────────────────────────────────────────────────────────

fn gui_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every `.rs` file under `src/`, excluding `src/theme/` — the one place a
/// colour is allowed to exist, because it is the palette's job to hold them.
fn view_sources() -> Vec<PathBuf> {
    let src = gui_root().join("src");
    let theme_dir = src.join("theme");
    let mut found = Vec::new();
    let mut stack = vec![src];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", current.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                if path != theme_dir {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Find where a raw string (`r"…"`, `r#"…"#`, `br##"…"##`, …) starts, if `i`
/// is the beginning of one. Returns the index just past the opening quote and
/// the hash count, so the caller knows what terminator to look for.
fn raw_string_start(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut pos = i;
    if bytes.get(pos) == Some(&b'b') {
        pos += 1;
    }
    if bytes.get(pos) != Some(&b'r') {
        return None;
    }
    pos += 1;
    let mut hashes = 0usize;
    while bytes.get(pos) == Some(&b'#') {
        hashes += 1;
        pos += 1;
    }
    if bytes.get(pos) != Some(&b'"') {
        return None;
    }
    pos += 1;
    Some((pos, hashes))
}

/// Strip `//` line comments, `/* … */` block comments (nested), and string
/// literals (plain, byte, and raw) — everything a doc example or a panic
/// message could put one of this file's patterns inside without meaning it.
///
/// Newlines inside every stripped region are preserved so reported line
/// numbers stay accurate, exactly as `dependency_law.rs::strip_comments`
/// does for comments alone.
fn strip_comments_and_strings(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0usize;
    let mut block_depth = 0usize;

    while i < bytes.len() {
        if block_depth > 0 {
            if bytes[i..].starts_with(b"/*") {
                block_depth += 1;
                i += 2;
            } else if bytes[i..].starts_with(b"*/") {
                block_depth -= 1;
                i += 2;
            } else {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            block_depth = 1;
            i += 2;
            continue;
        }
        if let Some((content_start, hashes)) = raw_string_start(bytes, i) {
            let mut terminator = vec![b'"'];
            terminator.extend(std::iter::repeat_n(b'#', hashes));
            i = content_start;
            while i < bytes.len() && !bytes[i..].starts_with(terminator.as_slice()) {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + terminator.len()).min(bytes.len());
            continue;
        }
        let is_plain_string_start =
            bytes[i] == b'"' || (bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'"'));
        if is_plain_string_start {
            i += if bytes[i] == b'b' { 2 } else { 1 };
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Per-line "this line is inside a `#[cfg(test)] mod … { … }` block" flags.
///
/// Detected by watching for the attribute followed by a line that opens a
/// brace, then tracking brace depth (on the comment/string-stripped source,
/// so a brace inside a string can't desync the count) until it returns to the
/// depth the block opened at. This reliably covers the one shape this
/// codebase actually uses — `#[cfg(test)]` immediately above `mod tests {` —
/// which is every occurrence in `workspace/gui/src` as of this writing. It
/// would not catch a `#[cfg(test)]` on an individual `fn`, but this codebase
/// does not do that; see the module doc / final report for what was and
/// wasn't exercised.
fn test_module_line_flags(stripped: &str) -> Vec<bool> {
    let mut flags = Vec::new();
    let mut depth: i64 = 0;
    let mut test_entry_depth: Option<i64> = None;
    let mut pending_cfg_test = false;

    for line in stripped.lines() {
        if line.contains("#[cfg(test)]") {
            pending_cfg_test = true;
        }
        let opens = line.matches('{').count() as i64;
        let closes = line.matches('}').count() as i64;

        let was_in_test = test_entry_depth.is_some();
        depth += opens;
        if pending_cfg_test && test_entry_depth.is_none() && opens > 0 {
            test_entry_depth = Some(depth);
            pending_cfg_test = false;
        }
        depth -= closes;
        if let Some(entry) = test_entry_depth {
            if depth < entry {
                test_entry_depth = None;
            }
        }

        flags.push(was_in_test || test_entry_depth.is_some());
    }
    flags
}

/// Whether `line` contains `pattern` as a genuine hit for the colour-literal
/// scan, rather than as part of a return-type annotation.
///
/// `-> Hsla {` and `-> Rgba {` are function signatures whose body happens to
/// open on the same line — extremely common Rust style, and *not* a struct
/// literal. A struct literal never immediately follows `->`, so filtering
/// exactly that prefix removes a real false-positive class (`fn foo(..) ->
/// Hsla {`) without hand-picking which files are exempt.
fn colour_pattern_hit(line: &str, pattern: &str) -> bool {
    if pattern != "Hsla {" && pattern != "Rgba {" {
        return line.contains(pattern);
    }
    let mut search_from = 0usize;
    while let Some(rel) = line[search_from..].find(pattern) {
        let at = search_from + rel;
        let preceded_by_arrow = line[..at].trim_end().ends_with("->");
        if !preceded_by_arrow {
            return true;
        }
        search_from = at + pattern.len();
    }
    false
}

/// Colour-literal spellings a view must never contain. The palette
/// (`src/theme/palette.rs`) is the only place these are legitimate.
///
/// `gpui::transparent_black()` / `gpui::transparent_white()` are deliberately
/// **not** on this list: as substrings they do not match `"gpui::black()"` /
/// `"gpui::white()"` (the text between `gpui::` and the parens differs), so
/// they pass this scan without needing a special case — and doctrinally they
/// should, because "no colour at all" is not a colour choice.
const FORBIDDEN_COLOUR_PATTERNS: &[&str] = &[
    "hsla(",
    "hsl(",
    "rgba(",
    "rgb(",
    "gpui::white()",
    "gpui::black()",
    "gpui::red()",
    "gpui::green()",
    "gpui::blue()",
    "gpui::yellow()",
    "Hsla {",
    "Rgba {",
];

/// No view source file spells out a colour by hand.
///
/// This is the first half of `spec.rs`'s promise: "adding a theme is one JSON
/// file … no colour code anywhere." A view that can write `hsla(…)` or
/// `Hsla { … }` directly can always route around the palette under deadline
/// pressure, and the whole value of the restructure was making that
/// impossible rather than merely undocumented.
#[test]
fn no_view_contains_a_colour_literal() {
    let sources = view_sources();
    assert!(
        sources.len() > 10,
        "expected many source files under src/ (excluding src/theme/), found {} — this \
         test scanned nothing and would pass vacuously",
        sources.len()
    );

    let mut violations = Vec::new();
    for path in &sources {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let stripped = strip_comments_and_strings(&source);
        let test_flags = test_module_line_flags(&stripped);

        for (number, (line, in_test)) in stripped.lines().zip(test_flags.iter()).enumerate() {
            if *in_test {
                continue;
            }
            for pattern in FORBIDDEN_COLOUR_PATTERNS {
                if colour_pattern_hit(line, pattern) {
                    let display = path.strip_prefix(gui_root()).unwrap_or(path);
                    violations.push(format!(
                        "  {}:{}: contains `{pattern}` — {}",
                        display.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "a view must never spell out a colour: every colour a view can display has to be a \
         role reachable from the palette, or the restructure this test guards is only \
         decorative. Ask for a role in `src/theme/tokens.rs` (extend `ColourRoles`, \
         `KindColours`, `SyntaxColours` or `TrustStyle`), or — if the shape you need already \
         exists but under a different hue — add a step to the theme's ramp in \
         `src/theme/palette.rs`. Comments and string literals are stripped before this scan, \
         so these are real expressions, not prose. Found:\n{}",
        violations.join("\n")
    );
}

/// The literal argument of an `.opacity(` / `.alpha(` call, if the argument is
/// a bare number and nothing else.
///
/// # Why this is not "starts with a digit"
///
/// It was, and it produced a false positive that mattered:
/// `src/motion/declarative.rs` writes `el.opacity(1.0 - t)`, which begins with
/// `1.0` and is an *expression* — the fade-out half of an animation, driven by
/// the animation's own parameter. Flagging it would have pushed the author
/// either to disable the guard or to launder the arithmetic into a variable to
/// get past it, and a guard people route around is worse than no guard.
///
/// So the test asks the question it actually means: is the whole argument a
/// number? Anything with an operator, an identifier or a call in it is a
/// computed value, which is exactly what the rule wants people to write.
fn literal_alpha_argument(rest: &str) -> Option<&str> {
    let end = rest.find(')')?;
    let arg = rest[..end].trim();
    if arg.is_empty() {
        return None;
    }
    let numeric = arg
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == '_');
    numeric.then_some(arg)
}

/// Whether a literal alpha argument is one of the allowed identity endpoints.
fn is_identity_endpoint(arg: &str) -> bool {
    matches!(arg, "0.0" | "1.0" | "0" | "1")
}

/// No view invents a transparency value.
///
/// `src/theme/tokens.rs`'s `AlphaTokens` doc comment records the defect this
/// guards: a sweep once found fifty-one numeric `.opacity(N)` call sites
/// across sixteen distinct values, plus seven private per-file alpha
/// constants, because the design system named every colour and no
/// transparency — so a view that needed "the same colour, quieter" had
/// nowhere to get the number from and invented one. `.opacity(0.0)` and
/// `.opacity(1.0)` are allowed exactly: they are animation identity endpoints
/// (fully hidden / fully shown), not a tint choice, and appear throughout
/// `src/motion/declarative.rs` for exactly that reason.
#[test]
fn no_view_invents_a_transparency() {
    let sources = view_sources();
    let mut violations = Vec::new();

    for path in &sources {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let stripped = strip_comments_and_strings(&source);
        let test_flags = test_module_line_flags(&stripped);

        for (number, (line, in_test)) in stripped.lines().zip(test_flags.iter()).enumerate() {
            if *in_test {
                continue;
            }
            for method in [".opacity(", ".alpha("] {
                let mut search_from = 0usize;
                while let Some(rel) = line[search_from..].find(method) {
                    let at = search_from + rel;
                    let after = &line[at + method.len()..];
                    search_from = at + method.len();
                    let Some(arg) = literal_alpha_argument(after) else {
                        continue;
                    };
                    if is_identity_endpoint(arg) {
                        continue;
                    }
                    let display = path.strip_prefix(gui_root()).unwrap_or(path);
                    violations.push(format!(
                        "  {}:{}: {}",
                        display.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "a view must never write a numeric transparency literal: reach for a rung of \
         `AlphaTokens` (`src/theme/tokens.rs`) instead — `hairline`, `wash`, `tint`, `veil`, \
         `half`, or `dim` — via `cx.theme_ext().alpha`. `.opacity(0.0)` and `.opacity(1.0)` are \
         the only literals allowed, as animation identity endpoints. Found:\n{}",
        violations.join("\n")
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The bundle itself
// ─────────────────────────────────────────────────────────────────────────────

/// Every bundled theme parses, and the set of them is well-formed enough to
/// cycle through.
///
/// The typed [`ThemeLoadError`] exists precisely so a failure here names
/// *which* theme and *why* — asserting on `is_ok()` or a message string would
/// throw that structure away, which is what doctrine §4 objects to.
#[test]
fn bundled_themes_all_parse_and_have_unique_keys() {
    let specs = match parse_bundled() {
        Ok(specs) => specs,
        Err(ThemeLoadError::Malformed { index, source }) => {
            panic!("bundled theme #{index} did not parse as a ThemeSpec: {source}")
        }
        Err(ThemeLoadError::DuplicateKey { first, second, key }) => {
            panic!("bundled themes #{first} and #{second} share the key `{key}`")
        }
        Err(ThemeLoadError::Empty) => panic!("BUNDLED is empty; the app cannot start themeless"),
    };

    assert!(
        specs.len() >= 4,
        "expected at least 4 bundled themes (the cycle needs variety to be worth having), \
         found {}",
        specs.len()
    );

    let mut seen_keys = HashSet::new();
    for spec in &specs {
        assert!(!spec.key.is_empty(), "a bundled theme has an empty `key`");
        assert!(
            seen_keys.insert(spec.key.clone()),
            "key `{}` is not unique — `parse_bundled` should have already rejected this",
            spec.key
        );
        assert!(
            !spec.name.is_empty(),
            "theme `{}` has an empty `name`",
            spec.key
        );
        assert!(
            !spec.blurb.is_empty(),
            "theme `{}` has an empty `blurb`",
            spec.key
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The palette-coverage test — the load-bearing one
// ─────────────────────────────────────────────────────────────────────────────

/// An `Hsla`'s RGB, rounded to 4 decimal places and ignoring alpha, so an f32
/// round-trip through `Rgba::from(Hsla)` compares equal to itself and alpha
/// (applied per-use, per `Ramp::at`) never causes a false miss.
fn quantized_rgb(c: gpui::Hsla) -> (i32, i32, i32) {
    let rgba = gpui::Rgba::from(c);
    let round4 = |x: f32| (x * 10_000.0).round() as i32;
    (round4(rgba.r), round4(rgba.g), round4(rgba.b))
}

fn palette_rgb_set(p: &Palette) -> HashSet<(i32, i32, i32)> {
    p.all_colours().iter().map(|c| quantized_rgb(*c)).collect()
}

fn assert_from_palette(
    theme_key: &str,
    set: &HashSet<(i32, i32, i32)>,
    field: &str,
    colour: gpui::Hsla,
) {
    assert!(
        set.contains(&quantized_rgb(colour)),
        "{theme_key}: `{field}` (h={:.3} s={:.3} l={:.3}) is not a colour this theme's palette \
         can produce. Every resolved role colour must trace back to `Palette::all_colours()` — \
         see `src/theme/resolve.rs` — or a literal survived inside the theme layer itself, \
         which is the exact failure the restructure exists to close (see `palette.rs`'s module \
         doc: three independent copies of one `hsla(…)` literal, kept in agreement by a \
         comment).",
        colour.h,
        colour.s,
        colour.l
    );
}

/// Every resolved role colour is a colour the theme's own palette produced.
///
/// This is the assertion the whole restructure exists to make checkable.
/// Before it, `light_theme()` and `dark_theme()` were free functions that
/// could — and did — write a hand-picked `hsla(…)` anywhere in the middle of
/// otherwise-derived roles, and nothing distinguished "derived from the
/// palette" from "happened to look right" at the type level. This test does:
/// it rebuilds the exact set of colours the palette can produce and checks
/// every role against it, so a role assignment that reaches past the palette
/// fails here even though its type, field name and doc comment all look
/// exactly like every other role's.
///
/// Every colour field is named explicitly — no `..` — so that a field added
/// to `ColourRoles`, `TrustStyle`, `KindColours` or `SyntaxColours` without a
/// matching line here is a compile error, not a silently-skipped role.
#[test]
fn every_resolved_role_colour_comes_from_the_palette() {
    for spec in parse_bundled().expect("bundled themes parse; pinned by bundled_themes_all_parse_and_have_unique_keys")
    {
        let theme: NudoxThemeExt = resolve(&spec);
        let set = palette_rgb_set(&theme.palette);
        let key = spec.key.as_str();

        let ColourRoles {
            bg_sunken,
            bg_base,
            bg_raised,
            bg_overlay,
            bg_hover,
            bg_active,
            fg_default,
            fg_muted,
            fg_faint,
            accent,
            accent_hover,
            accent_text,
            accent_wash,
            accent_wash_hover,
            accent_fg_on,
            border_subtle,
            border_default,
            border_strong,
            ring,
            ok,
            ok_fg,
            warn,
            warn_fg,
            danger,
            danger_fg,
            info,
            info_fg,
            scrim,
        } = theme.colours;
        for (field, colour) in [
            ("bg_sunken", bg_sunken),
            ("bg_base", bg_base),
            ("bg_raised", bg_raised),
            ("bg_overlay", bg_overlay),
            ("bg_hover", bg_hover),
            ("bg_active", bg_active),
            ("fg_default", fg_default),
            ("fg_muted", fg_muted),
            ("fg_faint", fg_faint),
            ("accent", accent),
            ("accent_hover", accent_hover),
            ("accent_text", accent_text),
            ("accent_wash", accent_wash),
            ("accent_wash_hover", accent_wash_hover),
            ("accent_fg_on", accent_fg_on),
            ("border_subtle", border_subtle),
            ("border_default", border_default),
            ("border_strong", border_strong),
            ("ring", ring),
            ("ok", ok),
            ("ok_fg", ok_fg),
            ("warn", warn),
            ("warn_fg", warn_fg),
            ("danger", danger),
            ("danger_fg", danger_fg),
            ("info", info),
            ("info_fg", info_fg),
            ("scrim", scrim),
        ] {
            assert_from_palette(key, &set, field, colour);
        }

        let TrustTokens {
            local,
            synced,
            remote,
            stale,
        } = theme.trust;
        for (trust_name, style) in [
            ("local", local),
            ("synced", synced),
            ("remote", remote),
            ("stale", stale),
        ] {
            let TrustStyle {
                colour,
                fg_on,
                wash,
                badge_glyph: _,
                badge_label: _,
                hatched: _,
            } = style;
            assert_from_palette(key, &set, &format!("trust.{trust_name}.colour"), colour);
            assert_from_palette(key, &set, &format!("trust.{trust_name}.fg_on"), fg_on);
            assert_from_palette(key, &set, &format!("trust.{trust_name}.wash"), wash);
        }

        let KindColours {
            module,
            record,
            field,
            function,
            alias,
            trait_,
            impl_,
            enum_,
            variant,
            const_,
            static_,
            reexport,
            param,
            kind_fg_on,
        } = theme.kind_colours;
        for (name, colour) in [
            ("kinds.module", module),
            ("kinds.record", record),
            ("kinds.field", field),
            ("kinds.function", function),
            ("kinds.alias", alias),
            ("kinds.trait_", trait_),
            ("kinds.impl_", impl_),
            ("kinds.enum_", enum_),
            ("kinds.variant", variant),
            ("kinds.const_", const_),
            ("kinds.static_", static_),
            ("kinds.reexport", reexport),
            ("kinds.param", param),
            ("kinds.kind_fg_on", kind_fg_on),
        ] {
            assert_from_palette(key, &set, name, colour);
        }

        let SyntaxColours {
            kw,
            ty_name,
            ident,
            generic,
            fn_name,
            punct,
            string_lit,
            number_lit,
            comment,
            attr,
            macro_,
            boolean,
        } = theme.syntax;
        for (name, colour) in [
            ("syntax.kw", kw),
            ("syntax.ty_name", ty_name),
            ("syntax.ident", ident),
            ("syntax.generic", generic),
            ("syntax.fn_name", fn_name),
            ("syntax.punct", punct),
            ("syntax.string_lit", string_lit),
            ("syntax.number_lit", number_lit),
            ("syntax.comment", comment),
            ("syntax.attr", attr),
            ("syntax.macro_", macro_),
            ("syntax.boolean", boolean),
        ] {
            assert_from_palette(key, &set, name, colour);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Distinctness and ordering
// ─────────────────────────────────────────────────────────────────────────────

/// Every bundled theme differs from every other in its three most-visible
/// surface roles.
///
/// The cycle keybinding is only worth pressing if every press changes
/// something a reader would notice. Two themes agreeing on `bg_base`,
/// `fg_default` and `accent` — the page, the text, and the interactive hue —
/// are the same theme wearing two names, whatever their trust or kind hues
/// say. `resolve.rs::every_bundled_theme_constructs_and_differs_from_the_others`
/// already checks the whole `ColourRoles` struct is unequal; this test checks
/// the *load-bearing subset* so a future theme that copies another one's
/// three headline roles and changes only, say, `warn` cannot slip through the
/// weaker whole-struct check by having *something* differ.
#[test]
fn themes_differ_from_each_other_in_every_surface_role() {
    let specs = parse_bundled().expect("bundled themes parse");
    let themes: Vec<NudoxThemeExt> = specs.iter().map(resolve).collect();

    for i in 0..themes.len() {
        for j in (i + 1)..themes.len() {
            let (a, b) = (&themes[i], &themes[j]);
            assert_ne!(
                a.colours.bg_base, b.colours.bg_base,
                "`{}` and `{}` share the same bg_base",
                a.theme_key, b.theme_key
            );
            assert_ne!(
                a.colours.fg_default, b.colours.fg_default,
                "`{}` and `{}` share the same fg_default",
                a.theme_key, b.theme_key
            );
            assert_ne!(
                a.colours.accent, b.colours.accent,
                "`{}` and `{}` share the same accent",
                a.theme_key, b.theme_key
            );
        }
    }
}

/// The alpha ladder's rungs are strictly ascending and never so close two of
/// them read as the same tint.
///
/// `AlphaTokens`'s own doc comment says why the gap floor matters: the
/// sixteen values the ladder replaced collapsed onto six rungs with at most
/// 0.06 of error, which is "below the just-noticeable difference for a tint
/// over a mid surface." A floor of 0.04 keeps every rung on the correct side
/// of that line — two rungs closer than that would be two names for one
/// appearance, which defeats the point of having named rungs at all.
#[test]
fn alpha_ladder_rungs_are_ordered_and_distinct() {
    let al = AlphaTokens::STANDARD;
    let rungs: [(&str, f32); 6] = [
        ("hairline", al.hairline),
        ("wash", al.wash),
        ("tint", al.tint),
        ("veil", al.veil),
        ("half", al.half),
        ("dim", al.dim),
    ];

    const MIN_GAP: f32 = 0.04;
    for pair in rungs.windows(2) {
        let (name_a, a) = pair[0];
        let (name_b, b) = pair[1];
        assert!(
            b > a,
            "alpha ladder is not ascending: `{name_a}` ({a}) must be less than `{name_b}` ({b})"
        );
        assert!(
            b - a >= MIN_GAP,
            "`{name_a}` ({a}) and `{name_b}` ({b}) are only {:.3} apart, below the {MIN_GAP} \
             floor — two rungs this close are two names for one appearance",
            b - a
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The pure-Rust lookup helpers
// ─────────────────────────────────────────────────────────────────────────────

/// `theme_by_key` and `default_theme` are the pure-Rust front door tests and
/// helpers use instead of booting a GPUI app — this pins that they actually
/// work against the real bundle, not a fixture.
#[test]
fn theme_by_key_and_default_theme_resolve_real_bundled_themes() {
    let specs = parse_bundled().expect("bundled themes parse");
    let first = &specs[0];

    let by_key = theme_by_key(&first.key)
        .unwrap_or_else(|| panic!("theme_by_key(\"{}\") returned None for a bundled key", first.key));
    assert_eq!(by_key.theme_key.as_ref(), first.key);

    let default = default_theme();
    assert_eq!(
        default.theme_key.as_ref(),
        first.key,
        "default_theme() must be the first theme in cycle order"
    );

    assert!(
        theme_by_key("not-a-real-theme-key").is_none(),
        "theme_by_key must return None, not panic or substitute, for an unknown key"
    );
}
