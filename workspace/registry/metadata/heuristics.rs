//! Keyword-normalization heuristics — ported from lib.rs `categories/synonyms.rs`.
//!
//! Three public items:
//! - [`normalize_keyword`] — pure canonicalizer (no data files needed).
//! - [`Synonyms`] — loads `tag-synonyms.csv` and maps near-duplicate keywords to
//!   canonical forms with a vote-weighted relevance score.
//! - [`Specifics`] — loads `specific-keywords.txt` and `bland-keywords.txt` and
//!   exposes a DSL for keyword relations (specificity, combination, splitting, and
//!   blandness).

use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::{fmt, fs, io};

use smol_str::SmolStr;

// ---------------------------------------------------------------------------
// normalize_keyword
// ---------------------------------------------------------------------------

/// Canonicalize a raw keyword string.
///
/// Rules (in order):
/// 1. Fast-path: already lowercase-alphanumeric + hyphens → strip leading/trailing `-`.
/// 2. Split on whitespace, `_`, `-`; apply well-known token replacements (`C++` →
///    `cpp`, `iOS` → `ios`, plural acronym stripping `SSDs` → `SSD`, etc.).
/// 3. Split on `/`.
/// 4. Handle `%` → `percent`, `&` → ` and `.
/// 5. MiB/GiB suffix normalization (`iB` → lowercase).
/// 6. `iOS`-style (lowercase first, uppercase second) and `TeX`-style (UpLow-Up)
///    → fully lowercase.
/// 7. Numeric RFC/ISO/IEC/BCP prefix → insert hyphen (`rfc2119` → `rfc-2119`).
/// 8. `Script` token → lowercase.
/// 9. Kebab-case the joined tokens (non-alphanumeric runs → single `-`, trim).
/// 10. Truncate to 55 chars with `…` if over 65.
#[must_use]
pub fn normalize_keyword(k_input: &str) -> SmolStr {
	// Fast-path: already canonical.
	if k_input
		.as_bytes()
		.iter()
		.all(|&b| (b.is_ascii_lowercase() && b.is_ascii_alphanumeric()) || b == b'-')
	{
		return k_input.trim_matches('-').into();
	}

	let tokens: Vec<Cow<str>> = k_input
		.split(|c: char| c.is_ascii_whitespace() || c == '_' || c == '-')
		.map(|k| {
			Cow::Borrowed(
				match k.trim_end_matches("'s").trim_end_matches("\u{2019}s") {
					"I/O" => "io",
					"i/o" => "io",
					"C++" => "cpp",
					"C/C++" => "c-or-cpp",
					"c++" => "cpp",
					"OSes" => "os",
					"LaTeX" => "latex",
					"XeTeX" => "xetex",
					"CHAdeMO" => "chademo",
					other => other,
				},
			)
		})
		.flat_map(|k: Cow<str>| {
			// We need to split on '/' but can't flat_map a Cow directly into
			// owned strings without some care. Collect slash-splits as owned.
			let s: String = k.into_owned();
			s.split('/').map(|p| p.to_owned()).collect::<Vec<_>>()
		})
		.map(|k| {
			let mut k: Cow<str> = Cow::Owned(k);

			// Plural acronym: SSDs → SSD, APIs → API.
			{
				let bytes = k.as_bytes();
				let len = bytes.len();
				if len > 2 {
					let last = bytes[len - 1];
					let before_last = bytes[len - 2];
					if last == b's' && before_last.is_ascii_uppercase() {
						let trimmed = &k.as_ref()[..len - 1];
						k = Cow::Owned(trimmed.to_owned());
					}
				}
			}

			if k.contains('%') {
				k = Cow::Owned(k.replace('%', "percent"));
			}
			if k.contains('&') {
				k = Cow::Owned(k.replace('&', " and "));
			}

			// MiB, GiB → mib, gib
			if k.ends_with("iB") {
				k = Cow::Owned(k.to_ascii_lowercase());
			}

			// iOS-style (lower first, upper second) or TeX-style (UpLowerUp, len==3)
			{
				let mut chars = k.chars();
				if let (Some(first), Some(second)) = (chars.next(), chars.next()) {
					let like_ios =
						first.is_ascii_lowercase() && second.is_ascii_uppercase();
					let like_tex = k.len() == 3
						&& first.is_ascii_uppercase()
						&& second.is_ascii_lowercase()
						&& chars.next().is_some_and(|t| t.is_ascii_uppercase());
					if like_ios || like_tex {
						k = Cow::Owned(k.to_ascii_lowercase());
					}
				}
			}

			// rfc2119 → rfc-2119, iso8601 → iso-8601, etc.
			{
				let k_lower = k.to_ascii_lowercase();
				for prefix in ["rfc", "iso", "iec", "bcp"] {
					if let Some(rest) = k_lower.strip_prefix(prefix) {
						if rest.len() >= 2 && rest.bytes().all(|b| b.is_ascii_digit()) {
							k = Cow::Owned(format!("{prefix}-{rest}"));
						}
						break;
					}
				}
			}

			// "JavaScript", "TypeScript", etc.
			if k.contains("Script") {
				k = Cow::Owned(k.to_ascii_lowercase());
			}

			k
		})
		.filter(|k| !k.is_empty())
		.take(30)
		.collect();

	// Join all tokens with spaces then kebab-case inline.
	let joined = tokens.join(" ");
	let kebab = inline_kebab_case(&joined);

	// TODO: deunicode fallback
	let mut res = SmolStr::from(kebab.trim_matches('-'));

	if res.len() > 65 {
		if let Some(truncated) = res.get(..55) {
			let mut s = truncated.to_string();
			s.push('…');
			res = SmolStr::from(s);
		}
	}

	res
}

/// Inline kebab-caser: lowercase, replace runs of non-alphanumeric bytes with a
/// single `-`, trim leading/trailing `-`.
///
/// This replaces `heck::AsKebabCase` which handles Unicode segmentation;
/// our data is overwhelmingly ASCII so this is sufficient.
fn inline_kebab_case(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	let mut in_sep = true; // start true to suppress leading dash
	for b in s.bytes() {
		if b.is_ascii_alphanumeric() {
			out.push(b.to_ascii_lowercase() as char);
			in_sep = false;
		} else {
			if !in_sep && !out.is_empty() {
				out.push('-');
				in_sep = true;
			}
		}
	}
	// Trim trailing '-'
	while out.ends_with('-') {
		out.pop();
	}
	out
}

// ---------------------------------------------------------------------------
// Synonyms
// ---------------------------------------------------------------------------

/// Synonym table loaded from `tag-synonyms.csv`.
///
/// Format (one entry per line, `#` comments ignored):
/// ```text
/// find,replace,score
/// ```
/// where `score` is `0..=5` (votes). Absent file → empty map (no panic).
pub struct Synonyms {
	mapping: HashMap<SmolStr, (SmolStr, u8)>,
}

impl Synonyms {
	/// Load from `data_dir/tag-synonyms.csv`. Returns an empty map if the file
	/// is absent.
	#[cold]
	pub fn new(data_dir: &Path) -> io::Result<Self> {
		let path = data_dir.join("tag-synonyms.csv");
		if !path.exists() {
			tracing::warn!("tag-synonyms.csv not found at {:?}; using empty map", path);
			return Ok(Self { mapping: HashMap::new() });
		}
		let lines = fs::read_to_string(&path)?;
		let mut mapping = HashMap::<SmolStr, (SmolStr, u8)>::with_capacity(2500);
		let mut needs_fixing = false;

		for l in lines.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
			let fail = || io::Error::new(io::ErrorKind::InvalidData, format!("synonyms error at: {l}"));
			let mut cols = l.splitn(3, ',');
			let find = SmolStr::from(cols.next().ok_or_else(fail)?);
			let replace = SmolStr::from(cols.next().ok_or_else(fail)?);
			let score: u8 = cols
				.next()
				.and_then(|p| p.parse().ok())
				.ok_or_else(fail)?;

			if score > 5 {
				tracing::error!("synonym borked score: {}", l);
			}

			match mapping.entry(find) {
				Entry::Occupied(mut e) => {
					if e.get().1 < score {
						tracing::error!("duplicate synonym {} and {}", l, e.get().0);
						needs_fixing = true;
						e.insert((replace, score));
					}
				}
				Entry::Vacant(e) => {
					e.insert((replace, score));
				}
			}
		}

		// Loop detection (debug only in original; kept as runtime-safe check).
		let mut to_remove = Vec::new();
		for k in mapping.keys() {
			let mut s: &str = k.as_str();
			let count = std::iter::from_fn(|| {
				s = mapping.get(s)?.0.as_str();
				Some(s)
			})
			.take(21)
			.count();
			if count >= 20 {
				tracing::error!("synonym loop at {},{}", k, s);
				needs_fixing = true;
				to_remove.push(k.clone());
			}
		}
		for k in to_remove {
			mapping.remove(&k);
		}

		if needs_fixing {
			tracing::warn!("synonym table had issues; consider regenerating tag-synonyms.csv");
		}

		Ok(Self { mapping })
	}

	/// Look up a keyword, returning `(canonical_tag, relevance)` where relevance
	/// is in `(0.0, 1.0]`. Returns `None` if not in the map.
	#[inline]
	#[must_use]
	pub fn get(&self, keyword: &str) -> Option<(&str, f32)> {
		let (tag, votes) = self.mapping.get(keyword)?;
		let relevance = (f32::from(*votes) / 5.0 + 0.1).min(1.0);
		Some((tag.as_str(), relevance))
	}

	fn get_matching(&self, keyword: &str, min_votes: u8) -> Option<(&str, f32)> {
		let (tag, votes) = self.mapping.get(keyword)?;
		if *votes >= min_votes {
			return Some((tag.as_str(), f32::from(*votes) / 5.0));
		}
		None
	}

	/// Follow the synonym chain up to two hops, normalizing both halves of
	/// hyphenated compounds recursively.
	#[must_use]
	pub fn max_normalize<'a>(&'a self, keyword: &'a str) -> Cow<'a, str> {
		self.max_normalize_inner(keyword, 2)
	}

	fn max_normalize_inner<'a>(&'a self, keyword: &'a str, mut depth: u8) -> Cow<'a, str> {
		let mut keyword: Cow<str> = Cow::Borrowed(
			self.mapping
				.get(keyword)
				.map(|(k, _)| {
					self.mapping
						.get(k.as_str())
						.map(|(k2, _)| k2.as_str())
						.unwrap_or(k.as_str())
				})
				.unwrap_or(keyword),
		);
		if depth == 0 {
			return keyword;
		}
		depth -= 1;

		let mut has_multiple_hyphens = false;
		if let Some((start, end)) = keyword.split_once('-') {
			if end.contains('-') {
				has_multiple_hyphens = true;
			}
			let start2 = self.max_normalize_inner(start, depth);
			let end2 = self.max_normalize_inner(end, depth);
			if start2 != start || end2 != end {
				keyword = if start2 != end2 {
					format!("{start2}-{end2}").into()
				} else {
					start2.to_string().into()
				};
			}
		}
		if has_multiple_hyphens {
			if let Some((start, end)) = keyword.rsplit_once('-') {
				let start2 = self.max_normalize_inner(start, depth);
				let end2 = self.max_normalize_inner(end, depth);
				if start2 != start || end2 != end {
					keyword = format!("{start2}-{end2}").into();
				}
			}
		}
		keyword
	}

	/// Normalize with a minimum vote threshold, following the chain at most two
	/// hops. Returns `(canonical, weight)`.
	#[must_use]
	pub fn normalize<'a>(&'a self, keyword: &'a str, min_votes: u8) -> (&'a str, f32) {
		debug_assert!(min_votes > 0 && min_votes <= 5);
		if let Some((alt, w1)) = self.get_matching(keyword, min_votes.min(5)) {
			if let Some((alt2, w2)) = self.get_matching(alt, (min_votes + 1).clamp(4, 5)) {
				return (alt2, w1 * w2);
			}
			return (alt, w1);
		}
		(keyword, 1.0)
	}

	/// Like [`normalize`] but returns a `SmolStr`, consuming the input if it
	/// does not map.
	#[inline]
	#[must_use]
	pub fn map_normalize(&self, word: SmolStr, min_votes: u8) -> SmolStr {
		if let Some((w, _)) = self.get_matching(word.as_str(), min_votes) {
			w.into()
		} else {
			word
		}
	}

	/// Iterate over all `(canonical, votes)` pairs in the map.
	pub fn dump_values(&self) -> impl Iterator<Item = &(SmolStr, u8)> {
		self.mapping.values()
	}
}

// ---------------------------------------------------------------------------
// Specifics DSL
// ---------------------------------------------------------------------------

/// The action half of a specifics relation entry.
#[derive(Debug, Clone, Copy)]
pub enum SpecificsAction<'a> {
	/// `& +other` — this keyword is more specific when paired with `other`.
	Increase(SpecificsCond<'a>),
	/// `& -other` — this keyword is less specific when paired with `other`.
	Decrease(SpecificsCond<'a>),
	/// `< other` — this keyword is subsumed by `other`.
	Fold(SpecificsCond<'a>),
}

impl<'a> SpecificsAction<'a> {
	/// The condition payload regardless of action kind.
	#[must_use]
	pub fn cond(&self) -> &SpecificsCond<'a> {
		let (Self::Increase(w) | Self::Decrease(w) | Self::Fold(w)) = self;
		w
	}

	/// The keyword this action references.
	#[must_use]
	pub fn keyword(&self) -> &'a str {
		self.cond().with
	}
}

/// A keyword together with optional `?include`/`!exclude` condition suffixes.
#[derive(Clone, Copy)]
pub struct SpecificsCond<'a> {
	/// The keyword itself.
	pub with: &'a str,
	/// The raw `?word!word…` tail (may be empty).
	include_exclude: &'a str,
}

impl fmt::Debug for SpecificsCond<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.with)?;
		f.write_str(self.include_exclude)
	}
}

impl<'a> SpecificsCond<'a> {
	fn new_plain(keyword: &'a str) -> Self {
		Self { with: keyword, include_exclude: "" }
	}

	fn new(action: &'a str) -> Self {
		debug_assert!(!action.starts_with(['+', '-']));
		let (with, include_exclude) = action
			.split_once(['!', '?'])
			.and_then(|(w, _)| action.split_at_checked(w.len()))
			.unwrap_or((action, ""));
		Self { with, include_exclude }
	}

	/// Keywords that must NOT be present.
	pub fn unless(&self) -> impl Iterator<Item = &str> {
		self.conditions().filter_map(|(id, w)| (id == b'!').then_some(w))
	}

	/// Keywords that must ALL be present.
	pub fn only_if_all(&self) -> impl Iterator<Item = &str> {
		self.conditions().filter_map(|(id, w)| (id == b'?').then_some(w))
	}

	/// Whether the conditions are satisfied given `cb` (returns `true` if the
	/// keyword is present in the current context).
	pub fn matches(&self, cb: impl Fn(&str) -> bool + Copy) -> bool {
		self.only_if_all().all(cb) && !self.unless().any(cb)
	}

	fn conditions(&self) -> impl Iterator<Item = (u8, &str)> {
		let mut rest = self.include_exclude;
		std::iter::from_fn(move || {
			let (id, r) = rest.split_at_checked(1)?;
			let (word, r) = r
				.as_bytes()
				.iter()
				.position(|&b| b == b'?' || b == b'!')
				.and_then(|pos| r.split_at_checked(pos))
				.unwrap_or((r, ""));
			rest = r;
			Some((id.as_bytes()[0], word))
		})
	}
}

/// Split-point offsets: up to 4 dot-positions stored as byte lengths.
type SplitOffsets = Vec<u8>;

/// The loaded specifics tables.
#[derive(Default)]
pub struct Specifics {
	/// `a + b c` — `a` is a combination of `b` and `c`.
	combine: HashMap<SmolStr, String>,
	/// `a < b` / `a & +b -c` — specificity/fold relations.
	relate: HashMap<SmolStr, String>,
	/// `a.b.c` — `a-b-c` splits into three parts at the stored offsets.
	split: HashMap<SmolStr, SplitOffsets>,
	/// Keywords that are too generic to be useful on their own.
	bland: HashSet<SmolStr>,
}

impl Specifics {
	/// Load from `data_dir/{specific-keywords.txt,bland-keywords.txt}`.
	/// Absent files → empty tables (no panic).
	#[inline(never)]
	pub fn new(data_dir: &Path) -> io::Result<Self> {
		let mut parsed = Self::default();

		let specifics_path = data_dir.join("specific-keywords.txt");
		let bland_path = data_dir.join("bland-keywords.txt");

		let specifics_src = if specifics_path.exists() {
			fs::read_to_string(&specifics_path)?
		} else {
			tracing::warn!("specific-keywords.txt not found at {:?}", specifics_path);
			String::new()
		};
		let bland_src = if bland_path.exists() {
			fs::read_to_string(&bland_path)?
		} else {
			tracing::warn!("bland-keywords.txt not found at {:?}", bland_path);
			String::new()
		};

		let errors = parsed.parse_keywords(&specifics_src, &bland_src);
		if errors {
			tracing::error!("Found syntax errors in specific-keywords.txt");
		}

		Ok(parsed)
	}

	/// Parse `lines` (specific-keywords.txt) and `bland` (bland-keywords.txt)
	/// into the internal tables. Returns `true` if any syntax errors were found.
	fn parse_keywords(&mut self, lines: &str, bland: &str) -> bool {
		let mut errors = false;

		for line in lines.lines() {
			let line = line.trim_ascii();

			let ((keyword_str, rest), must_have_prefix, list) =
				if let Some(parts) = line.split_once(" < ") {
					(parts, false, &mut self.relate)
				} else if let Some(parts) = line.split_once(" & ") {
					(parts, true, &mut self.relate)
				} else if let Some(parts) = line.split_once(" + ") {
					(parts, false, &mut self.combine)
				} else if line.contains('.') {
					// split entry: "a.b.c" → key "a-b-c", offsets [1, 1, ...]
					let mut prev = 0usize;
					let splits: Vec<u8> = line
						.as_bytes()
						.iter()
						.enumerate()
						.filter_map(|(i, &ch)| (ch == b'.').then_some(i))
						.take(4)
						.map(|n| {
							let len = (n - prev) as u8;
							prev = n + 1;
							len
						})
						.collect();

					let keyword = line.replace('.', "-");
					if splits.is_empty() || keyword.is_empty() {
						tracing::error!("bad split line: {}", line);
						errors = true;
						continue;
					}
					if self.split.insert(keyword.into(), splits).is_some() {
						tracing::error!("duplicate split line: {}", line);
						errors = true;
					}
					continue;
				} else {
					if !line.is_empty() && !line.starts_with('#') {
						tracing::error!("bad line: {}", line);
						errors = true;
					}
					continue;
				};

			let keyword = SpecificsCond::new(keyword_str.trim_ascii_end());
			if keyword.with.is_empty() {
				tracing::error!("bad line (empty keyword): {}", line);
				errors = true;
				continue;
			}

			let list = list.entry(SmolStr::from(keyword.with)).or_default();
			list.reserve(rest.trim_ascii_start().len());

			for word in rest.split([' ', ',']).filter(|b| !b.is_empty()) {
				let stem = word
					.trim_start_matches(['-', '+'])
					.split(['!', '?'])
					.next()
					.unwrap_or_default();
				if stem.is_empty()
					|| stem == keyword.with
					|| must_have_prefix != word.starts_with(['-', '+'])
				{
					tracing::error!("bad word '{}' in line: {}", word, line);
					errors = true;
					continue;
				}
				if !list.is_empty() {
					list.push(',');
				}
				list.push_str(word);
				if !keyword.include_exclude.is_empty() {
					list.push_str(keyword.include_exclude);
				}
			}
		}

		self.bland.extend(
			bland.lines()
				.map(|l| l.trim())
				.filter(|l| !l.is_empty() && !l.starts_with('#'))
				.map(SmolStr::from),
		);

		errors
	}

	/// If `word` has a `+` combination entry, return the canonical key and an
	/// iterator over the sibling conditions.
	#[inline]
	#[must_use]
	pub fn get_combined<'s>(
		&'s self,
		word: &str,
	) -> Option<(&'s str, impl Iterator<Item = SpecificsCond<'s>> + 's)> {
		let (k, v) = self.combine.get_key_value(word)?;
		Some((k.as_str(), v.split(',').map(SpecificsCond::new)))
	}

	/// If `word` has a split entry, return:
	/// - the canonical (hyphenated) key,
	/// - the number of parts,
	/// - an iterator yielding each part.
	#[inline]
	#[must_use]
	pub fn get_splits<'s>(
		&'s self,
		word: &str,
	) -> Option<(&'s str, usize, impl Iterator<Item = &'s str> + 's)> {
		let (k, v) = self.split.get_key_value(word)?;
		let mut rest = k.as_str();
		let mut offsets = v.clone().into_iter().map(usize::from);
		let n_parts = 1 + v.len();
		Some((
			k.as_str(),
			n_parts,
			std::iter::from_fn(move || {
				if rest.is_empty() {
					return None;
				}
				if let Some(n) = offsets.next() {
					let (part, r) = rest.split_at_checked(n + 1)?;
					rest = r;
					let (part, _) = part.split_at_checked(n)?;
					Some(part)
				} else {
					let part = rest;
					rest = &rest[..0];
					Some(part)
				}
			}),
		))
	}

	/// If `word` has a relation (`<` / `&`) entry, return the canonical key and
	/// an iterator over its [`SpecificsAction`]s.
	#[inline]
	#[must_use]
	pub fn get_relations<'s>(
		&'s self,
		word: &str,
	) -> Option<(&'s str, impl Iterator<Item = SpecificsAction<'s>> + 's)> {
		let (k, v) = self.relate.get_key_value(word)?;
		Some((
			k.as_str(),
			v.split(',').filter_map(move |action| {
				let (prefix, suffix) = action.split_at_checked(1)?;
				Some(match prefix {
					"+" => SpecificsAction::Increase(SpecificsCond::new(suffix)),
					"-" => SpecificsAction::Decrease(SpecificsCond::new(suffix)),
					_ => SpecificsAction::Fold(SpecificsCond::new(action)),
				})
			}),
		))
	}

	/// Iterator over all keywords that appear as relation heads or as one half of
	/// a combination. Useful for building the full keyword universe.
	pub fn extra_keywords(&self) -> impl Iterator<Item = SmolStr> + '_ {
		self.relate
			.keys()
			.cloned()
			.chain(self.combine.iter().flat_map(|(k1, rest)| {
				rest.split(',').map(move |k2| {
					let k2 = k2
						.trim_start_matches(['+', '-'])
						.split(['!', '?'])
						.next()
						.unwrap();
					SmolStr::from(format!("{k1}-{k2}"))
				})
			}))
	}

	/// If `keyword` is in the bland set, return a [`SpecificsAction::Fold`]
	/// wrapping a plain condition.
	pub fn is_bland<'s>(&'s self, keyword: &str) -> Option<SpecificsAction<'s>> {
		self.bland
			.get(keyword)
			.map(|k| SpecificsAction::Fold(SpecificsCond::new_plain(k.as_str())))
	}

	/// Iterate over all bland keywords.
	pub fn all_bland_keywords(&self) -> impl Iterator<Item = &str> + '_ {
		self.bland.iter().map(|s| s.as_str())
	}
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	// --- normalize_keyword ---------------------------------------------------

	#[test]
	fn normalize_cpp() {
		assert_eq!(normalize_keyword("C++"), SmolStr::from("cpp"));
	}

	#[test]
	fn normalize_ios() {
		assert_eq!(normalize_keyword("iOS"), SmolStr::from("ios"));
	}

	#[test]
	fn normalize_async_unchanged() {
		assert_eq!(normalize_keyword("async"), SmolStr::from("async"));
	}

	#[test]
	fn normalize_hello_world() {
		assert_eq!(normalize_keyword("Hello World"), SmolStr::from("hello-world"));
	}

	#[test]
	fn normalize_rfc2119() {
		assert_eq!(normalize_keyword("rfc2119"), SmolStr::from("rfc-2119"));
	}

	#[test]
	fn normalize_javascript() {
		assert_eq!(normalize_keyword("JavaScript"), SmolStr::from("javascript"));
	}

	#[test]
	fn normalize_apis_plural() {
		assert_eq!(normalize_keyword("APIs"), SmolStr::from("api"));
	}

	// --- Specifics::split ---------------------------------------------------

	#[test]
	fn split() {
		let mut s = Specifics::default();
		let err = s.parse_keywords("f12345.ba-bazz\na.b\na1.b.c33333.d.e4", "");
		assert!(!err);

		let (_, n, mut i) = s.get_splits("f12345-ba-bazz").unwrap();
		assert_eq!(2, n);
		assert_eq!("f12345", i.next().unwrap());
		assert_eq!("ba-bazz", i.next().unwrap());
		assert_eq!(None, i.next());

		let (_, n, mut i) = s.get_splits("a-b").unwrap();
		assert_eq!(2, n);
		assert_eq!("a", i.next().unwrap());
		assert_eq!("b", i.next().unwrap());
		assert_eq!(None, i.next());

		let (_, n, mut i) = s.get_splits("a1-b-c33333-d-e4").unwrap();
		assert_eq!(5, n);
		assert_eq!("a1", i.next().unwrap());
		assert_eq!("b", i.next().unwrap());
		assert_eq!("c33333", i.next().unwrap());
		assert_eq!("d", i.next().unwrap());
		assert_eq!("e4", i.next().unwrap());
		assert_eq!(None, i.next());
	}

	// --- SpecificsCond ------------------------------------------------------

	#[test]
	fn s_cond() {
		let c = SpecificsCond::new("k?req1?req2!not1!not2?req3");
		assert_eq!("k", c.with);
		assert_eq!("?req1?req2!not1!not2?req3", c.include_exclude);
		let mut cc = c.conditions();
		assert_eq!((b'?', "req1"), cc.next().unwrap());
		assert_eq!((b'?', "req2"), cc.next().unwrap());
		assert_eq!((b'!', "not1"), cc.next().unwrap());
	}
}
