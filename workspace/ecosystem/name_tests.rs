//! Phase 1 acceptance tests:
//!   - Differential: new parse_name/render_canonical vs. the legacy
//!     canonicalize_* functions embedded verbatim as reference impls.
//!   - Round-trip law: parse → render → parse yields equal canonical + structure.
//!   - Rejection agreement on invalid names.
//!   - `symbol_roots` / `search_surface` correctness.

#[cfg(test)]
mod differential {
	use crate::{Language, LanguageExt as _};

	// ── Reference implementations (verbatim copies from the old heart/package/mod.rs) ──

	fn ref_canonicalize_crate(raw: &str) -> Option<String> {
		let valid = raw.starts_with(|c: char| c.is_ascii_alphanumeric())
			&& raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
		valid.then(|| raw.to_ascii_lowercase().replace('_', "-"))
	}

	fn ref_canonicalize_npm(raw: &str) -> Option<String> {
		let segment_ok = |segment: &str| {
			!segment.is_empty()
				&& !segment.starts_with('.')
				&& segment
					.chars()
					.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
		};
		let valid = match raw.strip_prefix('@') {
			Some(scoped) => match scoped.split_once('/') {
				Some((scope, name)) => {
					segment_ok(scope) && segment_ok(name) && !name.contains('/')
				}
				None => false,
			},
			None => segment_ok(raw) && !raw.contains('/'),
		};
		valid.then(|| raw.to_ascii_lowercase())
	}

	fn ref_canonicalize_pep503(raw: &str) -> Option<String> {
		let valid = raw.starts_with(|c: char| c.is_ascii_alphanumeric())
			&& raw.ends_with(|c: char| c.is_ascii_alphanumeric())
			&& raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
		valid.then(|| {
			raw.to_ascii_lowercase()
				.split(['-', '_', '.'])
				.filter(|run| !run.is_empty())
				.collect::<Vec<_>>()
				.join("-")
		})
	}

	fn ref_canonicalize_go_module(raw: &str) -> Option<String> {
		if raw.is_empty() || raw.starts_with('/') || raw.contains("..") {
			return None;
		}
		let valid = raw.chars().all(|c| {
			c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '~' | '+' | ':')
		});
		valid.then(|| raw.to_string())
	}

	fn ref_canonicalize_maven_artifact(raw: &str) -> Option<String> {
		let valid = raw.starts_with(|c: char| c.is_ascii_alphanumeric())
			&& raw.ends_with(|c: char| c.is_ascii_alphanumeric())
			&& raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
		valid.then(|| raw.to_ascii_lowercase())
	}

	fn ref_canonicalize_csharp(raw: &str) -> Option<String> {
		let valid = raw.starts_with(|c: char| c.is_ascii_alphanumeric())
			&& raw.ends_with(|c: char| c.is_ascii_alphanumeric())
			&& raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
		valid.then(|| raw.to_ascii_lowercase())
	}

	fn ref_canonicalize_nix_flake(raw: &str) -> Option<String> {
		if raw.is_empty()
			|| raw.starts_with('/')
			|| raw.ends_with('/')
			|| raw.contains("..")
			|| raw.matches('/').count() > 1
		{
			return None;
		}
		let valid = raw
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
			&& raw.starts_with(|c: char| c.is_ascii_alphanumeric());
		valid.then(|| raw.to_ascii_lowercase())
	}

	fn new_canonical(lang: Language, raw: &str) -> Option<String> {
		lang.spec().parse_name(raw).map(|n| lang.spec().render_canonical(&n))
	}

	// ── Rust fixtures ──────────────────────────────────────────────────────────

	const RUST_VALID: &[&str] = &[
		"serde",
		"serde_json",
		"serde-json",
		"tokio",
		"axum",
		"axum-extra",
		"Rocket",
		"HYPER",
		"tower-http",
		"actix-web",
		"clap",
		"anyhow",
		"thiserror",
		"tracing",
		"tracing-subscriber",
		"regex",
		"rand",
		"rayon",
		"crossbeam-channel",
		"diesel",
		"sqlx",
		"async-trait",
		"bytes",
		"futures",
		"pin-project",
		"dashmap",
		"once_cell",
		"lazy_static",
		"num-traits",
		"itertools",
		"indexmap",
		"hashbrown",
		"parking_lot",
		"uuid",
		"chrono",
		"time",
		"url",
		"reqwest",
		"hyper",
		"h2",
		"a1",
	];

	const RUST_INVALID: &[&str] = &[
		"",
		"-leading",
		"_leading",
		"has space",
		"unicode\u{e9}",
		"has/slash",
		"has.dot",
		"has@at",
	];

	#[test]
	fn rust_differential() {
		for &raw in RUST_VALID {
			let reference = ref_canonicalize_crate(raw);
			let new_val = new_canonical(Language::Rust, raw);
			assert_eq!(
				reference, new_val,
				"Rust canonical mismatch for {raw:?}: ref={reference:?} new={new_val:?}"
			);
		}
	}

	#[test]
	fn rust_rejection_agreement() {
		for &raw in RUST_INVALID {
			let reference = ref_canonicalize_crate(raw);
			let new_val = new_canonical(Language::Rust, raw);
			assert_eq!(reference, new_val, "Rust rejection disagreement for {raw:?}");
		}
		// Over-length names: `parse_name` in the ecosystem crate has no length limit
		// (that lives in `heart::PackageName::new`); both ref and new return Some here.
		// The length gate is tested separately in heart's PackageName tests.
	}

	// ── npm fixtures ───────────────────────────────────────────────────────────

	const NPM_VALID: &[&str] = &[
		"react",
		"react-dom",
		"lodash",
		"express",
		"axios",
		"typescript",
		"@types/node",
		"@types/react",
		"@babel/core",
		"@babel/preset-env",
		"@angular/core",
		"@vue/cli",
		"@nestjs/core",
		"@jest/core",
		"webpack",
		"vite",
		"rollup",
		"esbuild",
		"prettier",
		"eslint",
		"jest",
		"mocha",
		"chai",
		"sinon",
		"supertest",
		"dotenv",
		"uuid",
		"moment",
		"dayjs",
		"date-fns",
		"underscore",
		"ramda",
		"rxjs",
		"zod",
		"yup",
		"joi",
		"fastify",
		"koa",
		"hapi",
		"socket.io",
	];

	const NPM_INVALID: &[&str] = &[
		"",
		"@scope",           // scoped but no name
		"@scope/",          // trailing slash
		"@/name",           // empty scope
		"has space",
		".leading-dot",
		"scope/name",       // missing @
		"@scope/a/b",       // double slash after scope
		"unicode\u{e9}",
	];

	#[test]
	fn npm_differential() {
		for &raw in NPM_VALID {
			let reference = ref_canonicalize_npm(raw);
			let new_val = new_canonical(Language::Typescript, raw);
			assert_eq!(
				reference, new_val,
				"npm canonical mismatch for {raw:?}: ref={reference:?} new={new_val:?}"
			);
		}
	}

	#[test]
	fn npm_rejection_agreement() {
		for &raw in NPM_INVALID {
			let reference = ref_canonicalize_npm(raw);
			let new_val = new_canonical(Language::Typescript, raw);
			assert_eq!(
				reference, new_val,
				"npm rejection disagreement for {raw:?}"
			);
		}
	}

	// ── PyPI fixtures ──────────────────────────────────────────────────────────

	const PYPI_VALID: &[&str] = &[
		"requests",
		"numpy",
		"pandas",
		"scipy",
		"matplotlib",
		"typing-extensions",
		"typing_extensions",
		"Typing.Extensions",
		"Pillow",
		"Django",
		"Flask",
		"FastAPI",
		"sqlalchemy",
		"pydantic",
		"httpx",
		"aiohttp",
		"celery",
		"redis",
		"boto3",
		"botocore",
		"certifi",
		"charset-normalizer",
		"idna",
		"urllib3",
		"six",
		"pytz",
		"python-dateutil",
		"pyOpenSSL",
		"cryptography",
		"paramiko",
		"pytest",
		"pytest-cov",
		"coverage",
		"attrs",
		"click",
		"rich",
		"tqdm",
		"black",
		"mypy",
		"flake8",
	];

	const PYPI_INVALID: &[&str] = &[
		"",
		"-leading",
		".leading",
		"trailing-",
		"trailing.",
		"has space",
		"unicode\u{e9}",
		"has/slash",
	];

	#[test]
	fn pypi_differential() {
		for &raw in PYPI_VALID {
			let reference = ref_canonicalize_pep503(raw);
			let new_val = new_canonical(Language::Python, raw);
			assert_eq!(
				reference, new_val,
				"PyPI canonical mismatch for {raw:?}: ref={reference:?} new={new_val:?}"
			);
		}
	}

	#[test]
	fn pypi_rejection_agreement() {
		for &raw in PYPI_INVALID {
			let reference = ref_canonicalize_pep503(raw);
			let new_val = new_canonical(Language::Python, raw);
			assert_eq!(
				reference, new_val,
				"PyPI rejection disagreement for {raw:?}"
			);
		}
	}

	// ── Go fixtures ────────────────────────────────────────────────────────────

	const GO_VALID: &[&str] = &[
		"github.com/gorilla/mux",
		"github.com/gorilla/mux/v2",
		"github.com/gorilla/websocket",
		"github.com/gin-gonic/gin",
		"github.com/go-chi/chi/v5",
		"github.com/pkg/errors",
		"github.com/stretchr/testify",
		"go.uber.org/zap",
		"go.uber.org/atomic",
		"golang.org/x/sync",
		"golang.org/x/net",
		"golang.org/x/crypto",
		"google.golang.org/grpc",
		"gopkg.in/yaml.v3",
		"gopkg.in/check.v1",
		"github.com/spf13/cobra",
		"github.com/spf13/viper",
		"github.com/sirupsen/logrus",
		"github.com/mitchellh/mapstructure",
		"github.com/hashicorp/go-multierror",
		"github.com/hashicorp/vault/api",
		"github.com/davecgh/go-spew",
		"github.com/prometheus/client_golang/prometheus",
		"github.com/grpc-ecosystem/grpc-gateway/v2",
		"k8s.io/client-go",
		"k8s.io/apimachinery",
		"sigs.k8s.io/controller-runtime",
		"github.com/aws/aws-sdk-go-v2",
		"github.com/Azure/azure-sdk-for-go/sdk/azcore",
		"github.com/google/go-cmp/cmp",
		"github.com/dgraph-io/badger/v4",
		"github.com/redis/go-redis/v9",
		"github.com/jackc/pgx/v5",
		"github.com/go-sql-driver/mysql",
		"github.com/gorm.io/gorm",
		"github.com/nats-io/nats.go",
		"github.com/eclipse/paho.mqtt.golang",
		"github.com/google/uuid",
		"github.com/shopspring/decimal",
		"github.com/robfig/cron/v3",
	];

	const GO_INVALID: &[&str] = &[
		"",
		"/leading-slash",
		"has..double-dot",
		"has space",
		"unicode\u{e9}",
	];

	#[test]
	fn go_differential() {
		for &raw in GO_VALID {
			let reference = ref_canonicalize_go_module(raw);
			let new_val = new_canonical(Language::Go, raw);
			assert_eq!(
				reference, new_val,
				"Go canonical mismatch for {raw:?}: ref={reference:?} new={new_val:?}"
			);
		}
	}

	#[test]
	fn go_rejection_agreement() {
		for &raw in GO_INVALID {
			let reference = ref_canonicalize_go_module(raw);
			let new_val = new_canonical(Language::Go, raw);
			assert_eq!(
				reference, new_val,
				"Go rejection disagreement for {raw:?}"
			);
		}
	}

	// ── Maven (Java) fixtures ──────────────────────────────────────────────────
	// Note: the `groupId:artifactId` form is NEW (not in the legacy reference);
	// we only differential-test the bare-artifactId form that the legacy accepted.

	const MAVEN_BARE_VALID: &[&str] = &[
		"spring-core",
		"spring-context",
		"spring-boot",
		"junit",
		"log4j",
		"slf4j-api",
		"guava",
		"commons-lang3",
		"commons-io",
		"jackson-databind",
		"jackson-core",
		"jackson-annotations",
		"hibernate-core",
		"mybatis",
		"mysql-connector-j",
		"postgresql",
		"h2",
		"jedis",
		"lettuce-core",
		"kafka-clients",
		"netty-all",
		"grpc-core",
		"protobuf-java",
		"gson",
		"okhttp",
		"retrofit",
		"rxjava",
		"reactor-core",
		"micrometer-core",
		"resilience4j-core",
		"feign-core",
		"mockito-core",
		"assertj-core",
		"testng",
		"hamcrest-core",
		"lombok",
		"mapstruct",
		"flyway-core",
		"liquibase-core",
		"poi-ooxml",
	];

	const MAVEN_BARE_INVALID: &[&str] = &[
		"",
		"-leading",
		"trailing-",
		"has space",
		"unicode\u{e9}",
		"has/slash",
	];

	#[test]
	fn maven_bare_differential() {
		for &raw in MAVEN_BARE_VALID {
			let reference = ref_canonicalize_maven_artifact(raw);
			let new_val = new_canonical(Language::Java, raw);
			assert_eq!(
				reference, new_val,
				"Maven canonical mismatch for {raw:?}: ref={reference:?} new={new_val:?}"
			);
		}
	}

	#[test]
	fn maven_bare_rejection_agreement() {
		for &raw in MAVEN_BARE_INVALID {
			let reference = ref_canonicalize_maven_artifact(raw);
			let new_val = new_canonical(Language::Java, raw);
			assert_eq!(
				reference, new_val,
				"Maven rejection disagreement for {raw:?}"
			);
		}
	}

	// Maven colon form — accepted by new impl only; legacy would return None.
	#[test]
	fn maven_colon_form_new_capability() {
		let cases = &[
			("org.springframework:spring-core", "org.springframework:spring-core"),
			("org.springframework.boot:spring-boot-starter", "org.springframework.boot:spring-boot-starter"),
			("com.google.guava:guava", "com.google.guava:guava"),
			("junit:junit", "junit:junit"),
			("Org.Example:MyArtifact", "org.example:myartifact"),
		];
		for &(raw, expected) in cases {
			let got = new_canonical(Language::Java, raw).expect("colon form is valid");
			assert_eq!(got, expected, "Maven colon canonical for {raw:?}");
			// Legacy returns None for colon form — no stored IDs exist to collide.
			assert!(
				ref_canonicalize_maven_artifact(raw).is_none(),
				"Legacy should reject colon form {raw:?}"
			);
		}
		// Invalid colon forms rejected.
		for &raw in &["org.spring:-artifact", ":artifact", "group:"] {
			assert!(
				new_canonical(Language::Java, raw).is_none(),
				"Should reject malformed colon form {raw:?}"
			);
		}
	}

	// ── NuGet fixtures ─────────────────────────────────────────────────────────

	const NUGET_VALID: &[&str] = &[
		// Degenerate but legacy-accepted: consecutive dots must survive the
		// namespace split/join round-trip byte-identically.
		"A..B",
		"Newtonsoft.Json",
		"Microsoft.Extensions.DependencyInjection",
		"Microsoft.Extensions.Logging",
		"Microsoft.Extensions.Configuration",
		"Microsoft.AspNetCore.Mvc",
		"Microsoft.EntityFrameworkCore",
		"Serilog",
		"Serilog.Sinks.File",
		"Serilog.Sinks.Console",
		"AutoMapper",
		"AutoMapper.Extensions.Microsoft.DependencyInjection",
		"FluentValidation",
		"FluentValidation.AspNetCore",
		"MediatR",
		"Polly",
		"Dapper",
		"StackExchange.Redis",
		"NUnit",
		"xunit",
		"xunit.runner.visualstudio",
		"Moq",
		"FluentAssertions",
		"NLog",
		"log4net",
		"Hangfire",
		"Hangfire.Core",
		"Quartz",
		"RestSharp",
		"CsvHelper",
		"HtmlAgilityPack",
		"BouncyCastle.Cryptography",
		"System.Text.Json",
		"Microsoft.Data.SqlClient",
		"Npgsql",
		"MongoDB.Driver",
		"RabbitMQ.Client",
		"MassTransit",
		"Refit",
		"Mapster",
		"BenchmarkDotNet",
	];

	const NUGET_INVALID: &[&str] = &[
		"",
		"-leading",
		"trailing-",
		".leading-dot",
		"trailing.dot.",
		"has space",
		"unicode\u{e9}",
		"has/slash",
		"has@at",
	];

	#[test]
	fn nuget_differential() {
		for &raw in NUGET_VALID {
			let reference = ref_canonicalize_csharp(raw);
			let new_val = new_canonical(Language::CSharp, raw);
			assert_eq!(
				reference, new_val,
				"NuGet canonical mismatch for {raw:?}: ref={reference:?} new={new_val:?}"
			);
		}
	}

	#[test]
	fn nuget_rejection_agreement() {
		for &raw in NUGET_INVALID {
			let reference = ref_canonicalize_csharp(raw);
			let new_val = new_canonical(Language::CSharp, raw);
			assert_eq!(
				reference, new_val,
				"NuGet rejection disagreement for {raw:?}"
			);
		}
	}

	// ── Nix fixtures ───────────────────────────────────────────────────────────

	const NIX_VALID: &[&str] = &[
		"NixOS/nixpkgs",
		"nixos/nixpkgs",
		"numtide/flake-utils",
		"nix-community/home-manager",
		"nix-community/nixd",
		"nix-community/rust-overlay",
		"cachix/cachix",
		"cachix/devenv",
		"hercules-ci/flake-parts",
		"ipetkov/crane",
		"oxalica/rust-overlay",
		"divnix/std",
		"flake-utils",
		"nixpkgs",
		"home-manager",
		"devenv",
		"rust-overlay",
		"crane",
		"flake-parts",
		"treefmt-nix",
	];

	const NIX_INVALID: &[&str] = &[
		"",
		"/leading",
		"trailing/",
		"a/b/c",           // two slashes
		"has..double-dot",
		"has space",
		"unicode\u{e9}",
	];

	#[test]
	fn nix_differential() {
		for &raw in NIX_VALID {
			let reference = ref_canonicalize_nix_flake(raw);
			let new_val = new_canonical(Language::Nix, raw);
			assert_eq!(
				reference, new_val,
				"Nix canonical mismatch for {raw:?}: ref={reference:?} new={new_val:?}"
			);
		}
	}

	#[test]
	fn nix_rejection_agreement() {
		for &raw in NIX_INVALID {
			let reference = ref_canonicalize_nix_flake(raw);
			let new_val = new_canonical(Language::Nix, raw);
			assert_eq!(
				reference, new_val,
				"Nix rejection disagreement for {raw:?}"
			);
		}
	}
}

#[cfg(test)]
mod round_trip {
	use crate::{Language, LanguageExt as _};

	fn check(lang: Language, raw: &str) {
		let spec = lang.spec();
		let Some(n1) = spec.parse_name(raw) else { return };
		let canonical1 = spec.render_canonical(&n1);
		let Some(n2) = spec.parse_name(&canonical1) else {
			panic!("render_canonical({raw:?}) = {canonical1:?} did not re-parse for {lang:?}");
		};
		let canonical2 = spec.render_canonical(&n2);
		assert_eq!(
			canonical1, canonical2,
			"Round-trip canonical mismatch for {lang:?} {raw:?}"
		);
		assert_eq!(n1.namespace, n2.namespace, "namespace drift for {lang:?} {raw:?}");
		assert_eq!(n1.name, n2.name, "name drift for {lang:?} {raw:?}");
		assert_eq!(n1.authority, n2.authority, "authority drift for {lang:?} {raw:?}");
		assert_eq!(n1.major, n2.major, "major drift for {lang:?} {raw:?}");
	}

	#[test]
	fn round_trips_all_ecosystems() {
		// Rust
		for &raw in &["serde", "serde_json", "TOKIO", "axum-extra", "a1"] {
			check(Language::Rust, raw);
		}
		// npm
		for &raw in &["react", "@types/node", "@babel/core", "lodash"] {
			check(Language::Typescript, raw);
		}
		// PyPI
		for &raw in &["requests", "Typing.Extensions", "typing_extensions", "my-pkg"] {
			check(Language::Python, raw);
		}
		// Go
		for &raw in &[
			"github.com/gorilla/mux",
			"github.com/go-chi/chi/v5",
			"go.uber.org/zap",
			"gopkg.in/yaml.v3",
		] {
			check(Language::Go, raw);
		}
		// Java bare
		for &raw in &["spring-core", "junit", "guava"] {
			check(Language::Java, raw);
		}
		// Java colon
		for &raw in &[
			"org.springframework:spring-core",
			"com.google.guava:guava",
			"junit:junit",
		] {
			check(Language::Java, raw);
		}
		// NuGet
		for &raw in &["Newtonsoft.Json", "Microsoft.Extensions.Logging", "Serilog"] {
			check(Language::CSharp, raw);
		}
		// Nix
		for &raw in &["NixOS/nixpkgs", "nixpkgs", "nix-community/home-manager"] {
			check(Language::Nix, raw);
		}
	}
}

#[cfg(test)]
mod symbol_roots_tests {
	use crate::{Language, LanguageExt as _};

	fn roots(lang: Language, raw: &str) -> Vec<String> {
		let spec = lang.spec();
		spec.parse_name(raw).expect("fixture is valid").symbol_roots()
	}

	// Rust: underscore root form
	#[test]
	fn rust_axum_root() {
		let r = roots(Language::Rust, "axum");
		assert_eq!(r, vec!["axum"]);
	}

	#[test]
	fn rust_serde_json_root() {
		let r = roots(Language::Rust, "serde-json");
		// canonical name is "serde-json"; symbol_roots converts to "serde_json"
		assert_eq!(r, vec!["serde_json"]);
	}

	#[test]
	fn rust_axum_must_not_match_axum_extra() {
		let axum_root = roots(Language::Rust, "axum");
		// "axum_extra::X" must NOT start with "axum" as a complete segment root.
		let fq = "axum_extra::Router";
		let root = &axum_root[0]; // "axum"
		// fq == root is false; strip_prefix gives "_extra::Router" which does not start with ':'|'.'|'/'
		let rest = fq.strip_prefix(root.as_str());
		assert!(
			rest.map(|r| !r.starts_with([':', '.', '/'])).unwrap_or(true),
			"axum root must not match axum_extra"
		);
	}

	// Go: full module path root
	#[test]
	fn go_gorilla_mux_root() {
		let r = roots(Language::Go, "github.com/gorilla/mux");
		assert_eq!(r, vec!["github.com/gorilla/mux"]);
	}

	#[test]
	fn go_chi_v5_root() {
		let r = roots(Language::Go, "github.com/go-chi/chi/v5");
		assert_eq!(r, vec!["github.com/go-chi/chi/v5"]);
	}

	// Java: dotted groupId prefix root
	#[test]
	fn java_spring_context_root() {
		let r = roots(Language::Java, "org.springframework:spring-context");
		assert_eq!(r, vec!["org.springframework"]);
	}

	#[test]
	fn java_bare_root() {
		let r = roots(Language::Java, "junit");
		assert_eq!(r, vec!["junit"]);
	}

	// npm: scoped form yields both @scope/name and bare name
	#[test]
	fn npm_scoped_roots() {
		let r = roots(Language::Typescript, "@types/node");
		assert!(r.contains(&"@types/node".to_string()), "scoped form must be in roots");
		assert!(r.contains(&"node".to_string()), "bare name must be in roots");
	}

	#[test]
	fn npm_bare_root() {
		let r = roots(Language::Typescript, "react");
		assert_eq!(r, vec!["react"]);
	}

	// NuGet: dotted ID reassembled
	#[test]
	fn nuget_newtonsoft_json_root() {
		let r = roots(Language::CSharp, "Newtonsoft.Json");
		assert_eq!(r, vec!["newtonsoft.json"]);
	}

	#[test]
	fn nuget_microsoft_extensions_root() {
		let r = roots(Language::CSharp, "Microsoft.Extensions.Logging");
		assert_eq!(r, vec!["microsoft.extensions.logging"]);
	}

	// Nix: project slug
	#[test]
	fn nix_nixos_nixpkgs_root() {
		let r = roots(Language::Nix, "NixOS/nixpkgs");
		assert_eq!(r, vec!["nixpkgs"]);
	}
}

#[cfg(test)]
mod search_surface_tests {
	use crate::{Language, LanguageExt as _};

	fn surface(lang: Language, raw: &str) -> String {
		lang.spec().parse_name(raw).expect("fixture is valid").search_surface()
	}

	/// R4: Go search surface must NOT contain `github` or `com` tokens.
	#[test]
	fn go_search_surface_strips_authority() {
		let s = surface(Language::Go, "github.com/gorilla/mux");
		let tokens: Vec<&str> = s.split_whitespace().collect();
		assert!(!tokens.contains(&"github"), "github must not appear in search surface");
		assert!(!tokens.contains(&"com"), "com must not appear in search surface");
		assert!(tokens.contains(&"gorilla"), "namespace should be in surface");
		assert!(tokens.contains(&"mux"), "name should be in surface");
	}

	#[test]
	fn go_bare_name_surface() {
		let s = surface(Language::Go, "example.org/mylib");
		assert!(!s.contains("example"), "authority must not appear");
		assert!(s.contains("mylib"));
	}

	#[test]
	fn rust_surface_is_just_name() {
		assert_eq!(surface(Language::Rust, "serde_json"), "serde-json");
	}

	#[test]
	fn npm_scoped_surface_contains_scope_and_name() {
		let s = surface(Language::Typescript, "@types/node");
		assert!(s.contains("types"));
		assert!(s.contains("node"));
	}

	#[test]
	fn nuget_dotted_surface_contains_all_segments() {
		let s = surface(Language::CSharp, "Microsoft.Extensions.Logging");
		assert!(s.contains("microsoft"));
		assert!(s.contains("extensions"));
		assert!(s.contains("logging"));
	}
}
