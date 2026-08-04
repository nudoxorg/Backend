//! EmbedTextBuilder v2 — the frozen text-construction recipe (09b §3).
//!
//! Both planes must build byte-identical embed texts from the same facets, or
//! local and remote vectors silently diverge (I1). The recipe id names this
//! exact construction; any change to the header format, section order, or
//! truncation budgets is a *new* recipe id and a full re-embed.

use heart::Language;

/// The frozen recipe identity, folded into every embed key (I1, I2).
pub const RECIPE_ID: &str = "nudox.embedtext.v2";

/// Total token budget per embed text (09b §3.1b, frozen).
pub const TOTAL_TOKEN_BUDGET: usize = 1024;

/// Hard cap on the signature section (09b §3.1b, frozen).
pub const SIG_TOKEN_CAP: usize = 512;

/// Cap on the doc section (09b §3.1b, frozen).
pub const DOC_TOKEN_CAP: usize = 256;

/// The named vector a text is built for — each symbol row carries up to three
/// vectors with different facet mixes (09b §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum VectorName {
	/// The full symbol view: header + signature + doc + body.
	Sym,
	/// The signature-only view: header + signature.
	Sig,
	/// The implementation view: lite header + body.
	Body,
}

impl VectorName {
	/// Stable wire/digest token.
	pub const fn as_str(self) -> &'static str {
		match self {
			VectorName::Sym => "sym",
			VectorName::Sig => "sig",
			VectorName::Body => "body",
		}
	}
}

/// Tokenization interface for budget enforcement. The *real* counter is the
/// model tokenizer (both planes ship the same one — I1); [`ApproxTokenCounter`]
/// is a deterministic fallback for tests and pre-flight estimates.
pub trait TokenCounter {
	/// Number of tokens in `text`.
	fn count(&self, text: &str) -> usize;

	/// A prefix of `text` containing at most `max_tokens` tokens, cut on a
	/// token boundary (never mid-token).
	fn truncate_to(&self, text: &str, max_tokens: usize) -> String;
}

/// Deterministic whitespace+punctuation token counter: a token is a maximal
/// run of alphanumeric/`_` chars, or a single other non-whitespace char.
/// Purely a heuristic stand-in — never use for real budget enforcement.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApproxTokenCounter;

impl ApproxTokenCounter {
	/// Byte spans of each token, in order.
	fn token_spans(text: &str) -> Vec<(usize, usize)> {
		let mut spans = Vec::new();
		let mut chars = text.char_indices().peekable();
		while let Some((start, c)) = chars.next() {
			if c.is_whitespace() {
				continue;
			}
			let mut end = start + c.len_utf8();
			if c.is_alphanumeric() || c == '_' {
				while let Some(&(j, d)) = chars.peek() {
					if d.is_alphanumeric() || d == '_' {
						end = j + d.len_utf8();
						chars.next();
					} else {
						break;
					}
				}
			}
			spans.push((start, end));
		}
		spans
	}
}

impl TokenCounter for ApproxTokenCounter {
	fn count(&self, text: &str) -> usize { Self::token_spans(text).len() }

	fn truncate_to(&self, text: &str, max_tokens: usize) -> String {
		let spans = Self::token_spans(text);
		if spans.len() <= max_tokens {
			return text.to_owned();
		}
		if max_tokens == 0 {
			return String::new();
		}
		text[..spans[max_tokens - 1].1].to_owned()
	}
}

/// The pre-extracted symbol facets an embed text is built from. All fields are
/// already normalized (single-space, comment markers stripped) by the caller;
/// this module only assembles and truncates.
#[derive(Debug, Clone, Copy)]
pub struct EmbedFacets<'a> {
	/// The symbol's language.
	pub language: Language,
	/// The package stem (name without version).
	pub package_stem: &'a str,
	/// The symbol kind tag (`"fn"`, `"struct"`, …).
	pub kind: &'a str,
	/// The stable cross-package moniker.
	pub moniker: &'a str,
	/// The in-package path.
	pub path: &'a str,
	/// The bare name.
	pub name: &'a str,
	/// The rendered signature.
	pub signature: &'a str,
	/// The normalized doc prose.
	pub doc: &'a str,
	/// The normalized body token stream.
	pub body_tokens: &'a str,
}

/// The owned twin of [`EmbedFacets`], for producers that assemble facets from
/// transient state (a stage callback cannot hand out borrows into itself).
/// Convert with [`EmbedFacetsBuf::as_facets`] at the recipe boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedFacetsBuf {
	pub language: Language,
	pub package_stem: String,
	pub kind: String,
	pub moniker: String,
	pub path: String,
	pub name: String,
	pub signature: String,
	pub doc: String,
	pub body_tokens: String,
}

impl EmbedFacetsBuf {
	/// Borrow as the recipe input type.
	pub fn as_facets(&self) -> EmbedFacets<'_> {
		EmbedFacets {
			language: self.language,
			package_stem: &self.package_stem,
			kind: &self.kind,
			moniker: &self.moniker,
			path: &self.path,
			name: &self.name,
			signature: &self.signature,
			doc: &self.doc,
			body_tokens: &self.body_tokens,
		}
	}
}

/// A finished, budget-conformant embed text. Newtype so raw strings cannot be
/// fed to an embedder as if they went through the recipe (I1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedText(String);

impl EmbedText {
	/// The recipe-built text.
	pub fn as_str(&self) -> &str { &self.0 }

	/// Unwrap for handoff to the embedder.
	pub fn into_inner(self) -> String { self.0 }
}

/// Header per 09b §3.1: comment lines carrying the identity facets. `Sym` and
/// `Sig` get the full header; `Body` gets the lite header (language, package,
/// kind, path) — the name/moniker live in the other two vectors.
fn header(facets: &EmbedFacets<'_>, vector: VectorName) -> String {
	let mut out = String::new();
	out.push_str(&format!("// language: {}\n", facets.language.as_token()));
	out.push_str(&format!("// package: {}\n", facets.package_stem));
	out.push_str(&format!("// kind: {}\n", facets.kind));
	match vector {
		VectorName::Sym | VectorName::Sig => {
			out.push_str(&format!("// moniker: {}\n", facets.moniker));
			out.push_str(&format!("// path: {}\n", facets.path));
			out.push_str(&format!("// name: {}\n", facets.name));
		}
		VectorName::Body => {
			out.push_str(&format!("// path: {}\n", facets.path));
		}
	}
	out
}

/// Build the embed text for one named vector under the frozen v2 budgets
/// (09b §3.1b):
///
/// - total budget [`TOTAL_TOKEN_BUDGET`] (1024);
/// - header + signature kept whole, but the signature alone is hard-truncated
///   at [`SIG_TOKEN_CAP`] (512);
/// - doc capped at [`DOC_TOKEN_CAP`] (256);
/// - body receives whatever budget remains, taken from the *start* of the
///   normalized stream, cut on a token boundary.
///
/// Facet mix: `Sym` = header+sig+doc+body, `Sig` = header+sig,
/// `Body` = lite-header+body. Empty sections are omitted entirely (no blank
/// lines), so absence is byte-stable (I1).
pub fn build_embed_text(
	facets: &EmbedFacets<'_>,
	vector: VectorName,
	counter: &dyn TokenCounter,
) -> EmbedText {
	let header = header(facets, vector);
	let header_tokens = counter.count(&header);

	let mut sections: Vec<String> = Vec::with_capacity(3);
	let mut used = header_tokens;

	// Signature (Sym + Sig): whole, but hard-capped at SIG_TOKEN_CAP.
	if matches!(vector, VectorName::Sym | VectorName::Sig) && !facets.signature.is_empty() {
		let sig = counter.truncate_to(facets.signature, SIG_TOKEN_CAP);
		used += counter.count(&sig);
		sections.push(sig);
	}

	// Doc (Sym only): capped at DOC_TOKEN_CAP.
	if vector == VectorName::Sym && !facets.doc.is_empty() {
		let doc = counter.truncate_to(facets.doc, DOC_TOKEN_CAP);
		used += counter.count(&doc);
		sections.push(doc);
	}

	// Body (Sym + Body): the remaining budget, from the stream start.
	if matches!(vector, VectorName::Sym | VectorName::Body) && !facets.body_tokens.is_empty() {
		let remaining = TOTAL_TOKEN_BUDGET.saturating_sub(used);
		let body = counter.truncate_to(facets.body_tokens, remaining);
		if !body.is_empty() {
			sections.push(body);
		}
	}

	let mut text = header;
	for section in &sections {
		text.push_str(section);
		text.push('\n');
	}
	EmbedText(text)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn facets<'a>(sig: &'a str, doc: &'a str, body: &'a str) -> EmbedFacets<'a> {
		EmbedFacets {
			language: Language::Rust,
			package_stem: "serde",
			kind: "fn",
			moniker: "serde::to_string",
			path: "serde/ser/to_string",
			name: "to_string",
			signature: sig,
			doc,
			body_tokens: body,
		}
	}

	/// A string of exactly `n` distinct word tokens under the approx counter.
	fn words(n: usize) -> String {
		(0..n).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ")
	}

	#[test]
	fn approx_counter_is_deterministic_and_boundary_exact() {
		let c = ApproxTokenCounter;
		// fn foo ( a : u32 ) - > bool — punctuation chars are single tokens.
		assert_eq!(c.count("fn foo(a: u32) -> bool"), 10);
		assert_eq!(c.count("a,b"), 3);
		assert_eq!(c.count("   "), 0);
		assert_eq!(c.count(""), 0);
		// truncate_to cuts on token boundary, never mid-token.
		assert_eq!(c.truncate_to("alpha beta gamma", 2), "alpha beta");
		assert_eq!(c.truncate_to("alpha beta", 5), "alpha beta");
		assert_eq!(c.truncate_to("alpha", 0), "");
		// A truncation is idempotent and count-consistent.
		let t = c.truncate_to(&words(100), 40);
		assert_eq!(c.count(&t), 40);
	}

	#[test]
	fn sig_over_512_tokens_is_hard_truncated() {
		let c = ApproxTokenCounter;
		let sig = words(600);
		let text = build_embed_text(&facets(&sig, "", ""), VectorName::Sig, &c);
		let total = c.count(text.as_str());
		let header_tokens = c.count(&header(&facets("", "", ""), VectorName::Sig));
		assert_eq!(total, header_tokens + SIG_TOKEN_CAP);
	}

	#[test]
	fn empty_doc_and_body_leave_no_blank_sections() {
		let c = ApproxTokenCounter;
		let text = build_embed_text(&facets("fn f()", "", ""), VectorName::Sym, &c);
		assert!(!text.as_str().contains("\n\n"), "no empty section artifacts: {:?}", text);
		assert!(text.as_str().ends_with("fn f()\n"));
	}

	#[test]
	fn total_budget_is_respected_at_exact_boundary() {
		let c = ApproxTokenCounter;
		// Big everything: sig 600 (→512), doc 300 (→256), body 2000 (→rest).
		let sig = words(600);
		let doc = words(300);
		let body = words(2000);
		let text = build_embed_text(&facets(&sig, &doc, &body), VectorName::Sym, &c);
		assert_eq!(c.count(text.as_str()), TOTAL_TOKEN_BUDGET);
	}

	#[test]
	fn body_gets_exactly_the_remaining_budget() {
		let c = ApproxTokenCounter;
		let sig = words(10);
		let doc = words(10);
		let body = words(5000);
		let f = facets(&sig, &doc, &body);
		let header_tokens = c.count(&header(&f, VectorName::Sym));
		let text = build_embed_text(&f, VectorName::Sym, &c);
		let body_budget = TOTAL_TOKEN_BUDGET - header_tokens - 10 - 10;
		// Body is taken from the start of the stream.
		assert!(text.as_str().contains("w0 w1 w2"));
		assert!(!text.as_str().contains(&format!("w{}", body_budget)));
		assert_eq!(c.count(text.as_str()), TOTAL_TOKEN_BUDGET);
	}

	#[test]
	fn header_exhausting_budget_yields_no_body() {
		let c = ApproxTokenCounter;
		let sig = words(SIG_TOKEN_CAP);
		let doc = words(DOC_TOKEN_CAP);
		// used ≈ header + 512 + 256; give a body and a doc that leave < body.
		let body = words(10_000);
		let text = build_embed_text(&facets(&sig, &doc, &body), VectorName::Sym, &c);
		assert_eq!(c.count(text.as_str()), TOTAL_TOKEN_BUDGET);
	}

	// ── adversarial: multi-byte UTF-8 corpus ─────────────────────────────────

	fn utf8_corpus() -> Vec<String> {
		vec![
			// CJK ideographs — 3 bytes each, char::is_alphanumeric() == true → whole
			// CJK run is ONE token.
			"你好世界 foo bar baz".to_owned(),
			// Emoji — 4 bytes, NOT alphanumeric → each is a single-char token.
			"🦀 🔥 💯 rust is great 🎉".to_owned(),
			// Combining grave (U+0301) — not alphanumeric, breaks the preceding run.
			"cafe\u{0301} au lait".to_owned(),
			// Mixed ASCII, CJK, emoji.
			"fn 解析(input: &str) -> Result<好, 错误> { /* 🦀 */ }".to_owned(),
			// Greek letters — alphanumeric, form one run.
			"αβγδ lambda(x) x+1".to_owned(),
			// Starts and ends with multi-byte.
			"你 a b c d 世".to_owned(),
		]
	}

	/// truncate_to must never split a char: output must be valid UTF-8 and a
	/// byte-level prefix of the original string.
	#[test]
	fn truncate_to_never_splits_multibyte_char() {
		let c = ApproxTokenCounter;
		for s in utf8_corpus() {
			let total = c.count(&s);
			for n in 0..=total + 1 {
				let truncated = c.truncate_to(&s, n);
				// valid UTF-8 (panics otherwise — caught by test harness)
				assert!(std::str::from_utf8(truncated.as_bytes()).is_ok(),
					"not valid UTF-8: input={:?} n={}", s, n);
				// must be a byte-level prefix of the original
				assert!(s.starts_with(truncated.as_str()),
					"not a prefix: input={:?} n={} out={:?}", s, n, truncated);
			}
		}
	}

	/// Property: count(truncate_to(s, n)) == min(count(s), n) over nasty corpus.
	#[test]
	fn truncate_to_count_property_multibyte() {
		let c = ApproxTokenCounter;
		for s in utf8_corpus() {
			let total = c.count(&s);
			for n in 0..=total + 2 {
				let truncated = c.truncate_to(&s, n);
				assert_eq!(c.count(&truncated), total.min(n),
					"count(truncate_to({:?}, {})) != min({}, {})", s, n, total, n);
			}
		}
	}

	// ── adversarial: empty sig with non-empty doc/body ────────────────────────

	#[test]
	fn empty_sig_sym_includes_doc_and_body() {
		let c = ApproxTokenCounter;
		let f = facets("", "docstring here", "body tokens here");
		let text = build_embed_text(&f, VectorName::Sym, &c);
		let s = text.as_str();
		assert!(s.contains("docstring here"), "doc present with empty sig");
		assert!(s.contains("body tokens here"), "body present with empty sig");
		assert!(!s.contains("\n\n"), "no blank lines: {:?}", s);
	}

	#[test]
	fn empty_sig_sig_vector_is_header_only() {
		let c = ApproxTokenCounter;
		let f = facets("", "some doc", "some body");
		let text = build_embed_text(&f, VectorName::Sig, &c);
		let s = text.as_str();
		assert!(!s.contains("some doc"), "Sig vector excludes doc");
		assert!(!s.contains("some body"), "Sig vector excludes body");
		// Header ends with "// name: to_string\n" — nothing after.
		assert!(s.ends_with("// name: to_string\n"),
			"header-only Sig ends at last header line: {:?}", s);
	}

	// ── adversarial: sig of exactly 512 and 513 tokens ───────────────────────

	#[test]
	fn sig_exactly_512_tokens_passes_untruncated() {
		let c = ApproxTokenCounter;
		let sig = words(SIG_TOKEN_CAP); // 512 words
		assert_eq!(c.count(&sig), SIG_TOKEN_CAP);
		let text = build_embed_text(&facets(&sig, "", ""), VectorName::Sig, &c);
		// Last word of a 512-word sequence is "w511".
		assert!(text.as_str().contains("w511"),
			"w511 (512th token) must survive: {:?}", text.as_str());
	}

	#[test]
	fn sig_513_tokens_truncated_to_512() {
		let c = ApproxTokenCounter;
		let sig = words(SIG_TOKEN_CAP + 1); // 513 words
		let text = build_embed_text(&facets(&sig, "", ""), VectorName::Sig, &c);
		// w512 is the 513th word — must be cut.
		assert!(!text.as_str().contains("w512"),
			"w512 (513th token) must be cut: {:?}", text.as_str());
		// w511 (512th word) must remain.
		assert!(text.as_str().contains("w511"),
			"w511 (512th token) must survive: {:?}", text.as_str());
	}

	// ── adversarial: header determinism ──────────────────────────────────────

	/// Identical facets → byte-identical EmbedText for every VectorName.
	#[test]
	fn header_determinism_byte_identical_all_vector_names() {
		let c = ApproxTokenCounter;
		let f = facets("fn foo(x: u32) -> bool", "doc comment here", "let x = 1;");
		for vector in [VectorName::Sym, VectorName::Sig, VectorName::Body] {
			let a = build_embed_text(&f, vector, &c);
			let b = build_embed_text(&f, vector, &c);
			assert_eq!(a.as_str(), b.as_str(),
				"EmbedText for {:?} must be byte-identical on re-call", vector);
		}
	}

	// ── adversarial: header-injection shaped input ────────────────────────────

	/// A sig field containing "\n// language: x" is appended verbatim — the
	/// recipe does not sanitize. Pin the EXACT current behavior.
	///
	/// FROZEN-KNOWN-BEHAVIOR: the recipe inserts the injected string literally.
	/// A consumer parsing EmbedText as structured headers would see a spurious
	/// comment line. This test pins the current behavior; if sanitization is
	/// added, update accordingly.
	#[test]
	fn header_injection_shaped_sig_pinned_frozen_behavior() {
		let c = ApproxTokenCounter;
		let injection = "fn evil()\n// language: evil\nfn inner()";
		let f = facets(injection, "", "");
		let text = build_embed_text(&f, VectorName::Sig, &c);
		let s = text.as_str();

		// Real header is first.
		assert!(s.starts_with("// language: rust\n"),
			"real language header must come first: {:?}", s);

		// The injected string appears verbatim (frozen behavior — not sanitized).
		assert!(s.contains("// language: evil"),
			"injected comment present verbatim (frozen): {:?}", s);

		// Real header precedes the injection.
		let real_pos = s.find("// language: rust").unwrap();
		let injected_pos = s.find("// language: evil").unwrap();
		assert!(real_pos < injected_pos,
			"real header before injection (frozen): real={} injected={}", real_pos, injected_pos);
	}

	#[test]
	fn facet_mix_per_vector_name() {
		let c = ApproxTokenCounter;
		let f = facets("SIGTOK", "DOCTOK", "BODYTOK");

		let sym = build_embed_text(&f, VectorName::Sym, &c);
		assert!(sym.as_str().contains("SIGTOK") && sym.as_str().contains("DOCTOK"));
		assert!(sym.as_str().contains("BODYTOK"));
		assert!(sym.as_str().contains("// moniker: serde::to_string"));

		let sig = build_embed_text(&f, VectorName::Sig, &c);
		assert!(sig.as_str().contains("SIGTOK"));
		assert!(!sig.as_str().contains("DOCTOK") && !sig.as_str().contains("BODYTOK"));

		let body = build_embed_text(&f, VectorName::Body, &c);
		assert!(body.as_str().contains("BODYTOK"));
		assert!(!body.as_str().contains("SIGTOK") && !body.as_str().contains("DOCTOK"));
		// Lite header: no moniker/name lines.
		assert!(!body.as_str().contains("// moniker:"));
		assert!(!body.as_str().contains("// name:"));
		assert!(body.as_str().contains("// language: rust"));
		assert!(body.as_str().contains("// path: serde/ser/to_string"));
	}
}
