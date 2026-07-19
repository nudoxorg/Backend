//! `PackageSelector::matches` tests covering all 7 ecosystems (Q2 acceptance).

mod common;

use heart::{Language, Name, Symbol, SymbolKind};
use server::search::query::PackageSelector;
use smol_str::SmolStr;

fn selector(lang: Language, raw: &str) -> PackageSelector {
	PackageSelector {
		name: server::registry::package::PackageName::new(lang, raw)
			.expect("fixture names are valid"),
		version: None,
	}
}

fn sym(lang: Language, fq: &str) -> Symbol {
	let plain = fq.split([':', '.', '/']).next_back().unwrap_or(fq);
	Symbol {
		id: common::absent_symbol_id(),
		package: common::package_id("fixture"),
		ecosystem: lang,
		name: Name { plain: SmolStr::new(plain), fully_qualified: SmolStr::new(fq) },
		kind: SymbolKind::Function,
	}
}

// ── Rust ──────────────────────────────────────────────────────────────────────

#[test]
fn rust_axum_matches_axum_router() {
	let sel = selector(Language::Rust, "axum");
	assert!(sel.matches(&sym(Language::Rust, "axum::Router")));
	assert!(sel.matches(&sym(Language::Rust, "axum")));
}

#[test]
fn rust_axum_does_not_match_axum_extra() {
	let sel = selector(Language::Rust, "axum");
	assert!(!sel.matches(&sym(Language::Rust, "axum_extra::X")));
}

#[test]
fn rust_serde_matches_serde_json_deserialization() {
	let sel = selector(Language::Rust, "serde");
	// serde root is "serde"; "serde::Deserialize" is prefixed by "serde:"
	assert!(sel.matches(&sym(Language::Rust, "serde::Deserialize")));
}

#[test]
fn rust_serde_does_not_match_serde_json() {
	let sel = selector(Language::Rust, "serde");
	// serde_json canonical is "serde-json", root is "serde_json"; "serde" must NOT match it.
	assert!(!sel.matches(&sym(Language::Rust, "serde_json::Value")));
}

// ── npm ───────────────────────────────────────────────────────────────────────

#[test]
fn npm_scoped_matches_fq_scoped() {
	let sel = selector(Language::Typescript, "@types/node");
	assert!(sel.matches(&sym(Language::Typescript, "@types/node")));
}

#[test]
fn npm_scoped_matches_bare_name() {
	let sel = selector(Language::Typescript, "@types/node");
	assert!(sel.matches(&sym(Language::Typescript, "node")));
}

#[test]
fn npm_bare_matches_bare_fq() {
	let sel = selector(Language::Typescript, "react");
	assert!(sel.matches(&sym(Language::Typescript, "react")));
}

#[test]
fn npm_wrong_ecosystem_rejected() {
	let sel = selector(Language::Typescript, "react");
	assert!(!sel.matches(&sym(Language::Rust, "react")));
}

// ── PyPI ──────────────────────────────────────────────────────────────────────

#[test]
fn python_requests_matches() {
	let sel = selector(Language::Python, "requests");
	assert!(sel.matches(&sym(Language::Python, "requests")));
}

#[test]
fn python_requests_matches_submodule() {
	let sel = selector(Language::Python, "requests");
	assert!(sel.matches(&sym(Language::Python, "requests.auth")));
}

#[test]
fn python_requests_does_not_match_requests_oauthlib() {
	let sel = selector(Language::Python, "requests");
	// "requests_oauthlib" → root "requests-oauthlib" ≠ "requests"
	// But as a symbol path "requests_oauthlib.OAuth1" — the root "requests_oauthlib"
	// won't match selector root "requests" because "requests_oauthlib" starts with
	// "requests_" not "requests:" or "requests."
	assert!(!sel.matches(&sym(Language::Python, "requests_oauthlib.OAuth1")));
}

// ── Go ────────────────────────────────────────────────────────────────────────

#[test]
fn go_gorilla_mux_matches() {
	let sel = selector(Language::Go, "github.com/gorilla/mux");
	assert!(sel.matches(&sym(Language::Go, "github.com/gorilla/mux.Router")));
}

#[test]
fn go_gorilla_mux_does_not_match_websocket() {
	let sel = selector(Language::Go, "github.com/gorilla/mux");
	assert!(!sel.matches(&sym(Language::Go, "github.com/gorilla/websocket.Conn")));
}

#[test]
fn go_chi_v5_matches() {
	let sel = selector(Language::Go, "github.com/go-chi/chi/v5");
	assert!(sel.matches(&sym(Language::Go, "github.com/go-chi/chi/v5.Router")));
}

// ── Java ──────────────────────────────────────────────────────────────────────

#[test]
fn java_spring_context_matches_application_context() {
	// Colon form: selector `org.springframework:spring-context`
	// root = "org.springframework"; fq "org.springframework.context.ApplicationContext"
	let sel = selector(Language::Java, "org.springframework:spring-context");
	assert!(sel.matches(&sym(Language::Java, "org.springframework.context.ApplicationContext")));
}

/// The groupId "org.springframework" correctly matches ALL spring submodule symbols —
/// this is the intended behaviour for Maven group-level scoping.
#[test]
fn java_spring_groupid_matches_any_spring_submodule() {
	let sel = selector(Language::Java, "org.springframework:spring-context");
	// "org.springframework.boot.SpringApplication" shares the group → matched.
	assert!(sel.matches(&sym(Language::Java, "org.springframework.boot.SpringApplication")));
	// A different group must NOT match.
	assert!(!sel.matches(&sym(Language::Java, "io.micrometer.core.instrument.Meter")));
}

#[test]
fn java_bare_artifact_matches() {
	let sel = selector(Language::Java, "junit");
	assert!(sel.matches(&sym(Language::Java, "junit")));
}

// ── NuGet ─────────────────────────────────────────────────────────────────────

#[test]
fn nuget_newtonsoft_json_matches_linq() {
	let sel = selector(Language::CSharp, "Newtonsoft.Json");
	assert!(sel.matches(&sym(Language::CSharp, "newtonsoft.json.linq.jobject")));
}

#[test]
fn nuget_newtonsoft_does_not_match_json_net() {
	// A hypothetical other package "Json.NET" should not match "Newtonsoft.Json"
	let sel = selector(Language::CSharp, "Newtonsoft.Json");
	assert!(!sel.matches(&sym(Language::CSharp, "json.net.something")));
}

// ── Nix ───────────────────────────────────────────────────────────────────────

#[test]
fn nix_nixpkgs_matches() {
	let sel = selector(Language::Nix, "NixOS/nixpkgs");
	assert!(sel.matches(&sym(Language::Nix, "nixpkgs")));
}

#[test]
fn nix_nixpkgs_does_not_match_home_manager() {
	let sel = selector(Language::Nix, "NixOS/nixpkgs");
	assert!(!sel.matches(&sym(Language::Nix, "home-manager")));
}
