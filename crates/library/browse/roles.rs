//! The role a dependency plays in a project, derived from what it says about
//! itself and who uses it. Nothing here knows a crate by name: a role is a
//! vote over the dependency's own crates.io categories and keywords, then the
//! role of a declared peer used by exactly the same members, then a telling
//! phrase in its description. The evidence is recorded, so a reader can see
//! why toml "speaks formats".
//!
//! The peer comes before the description because descriptions mislead:
//! rust-analyzer's `ra_ap_base_db` says "database", yet it is used only by the
//! Rust frontend, exactly as `tree-sitter-rust` is.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What a dependency does for this project, in display order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoleId {
    /// Test and fuzzing support.
    Tests,
    /// Windows, drawing and images.
    Window,
    /// Reading source languages: parsers, compilers, editors' grammars.
    Languages,
    /// Data formats and serialization.
    Formats,
    /// Storage, databases and search.
    Store,
    /// Async runtimes, futures and synchronization.
    Concurrency,
    /// HTTP and sockets.
    Network,
    /// Hashes, cryptography and compression.
    Hashing,
    /// Error types and reporting.
    Errors,
    /// Tracing, logging and metrics.
    Observe,
    /// Data structures and allocation.
    Memory,
    /// Operating-system and filesystem access.
    Os,
    /// Nothing on record says.
    Other,
}

impl RoleId {
    /// Every role, in the order a tree shows them.
    pub const ALL: [Self; 13] = [
        Self::Tests,
        Self::Window,
        Self::Languages,
        Self::Formats,
        Self::Store,
        Self::Concurrency,
        Self::Network,
        Self::Hashing,
        Self::Errors,
        Self::Observe,
        Self::Memory,
        Self::Os,
        Self::Other,
    ];

    /// The stable kebab-case spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tests => "tests",
            Self::Window => "window",
            Self::Languages => "languages",
            Self::Formats => "formats",
            Self::Store => "store",
            Self::Concurrency => "concurrency",
            Self::Network => "network",
            Self::Hashing => "hashing",
            Self::Errors => "errors",
            Self::Observe => "observe",
            Self::Memory => "memory",
            Self::Os => "os",
            Self::Other => "other",
        }
    }
}

/// Why a dependency landed in its role.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoleEvidence {
    /// Every edge to it is a dev-dependency.
    DevOnly,
    /// Its own crates.io categories and keywords voted, in rule order.
    Declared {
        /// The categories and keywords that voted for the role.
        words: Box<[String]>,
    },
    /// A telling phrase in its own description.
    Described {
        /// The phrase, as written in the description.
        phrase: String,
    },
    /// It declares nothing; a declared dependency used by exactly the same
    /// members, the same way, has this role.
    Cohort {
        /// That dependency's name.
        peer: String,
    },
    /// No category, keyword, telling description or peer.
    Unknown,
}

/// One vote rule: the categories and keywords that speak for a role.
struct Rule {
    role: RoleId,
    categories: &'static [&'static str],
    keywords: &'static [&'static str],
}

/// Checked in this order; on a tied vote the earlier rule wins.
const RULES: &[Rule] = &[
    Rule {
        role: RoleId::Tests,
        categories: &["development-tools::testing"],
        keywords: &[
            "testing",
            "test",
            "fixture",
            "fuzz",
            "fuzzing",
            "quickcheck",
            "property",
        ],
    },
    Rule {
        role: RoleId::Errors,
        categories: &[],
        keywords: &["error", "error-handling"],
    },
    Rule {
        role: RoleId::Observe,
        categories: &[
            "development-tools::debugging",
            "development-tools::profiling",
        ],
        keywords: &[
            "tracing",
            "logging",
            "telemetry",
            "metrics",
            "opentelemetry",
        ],
    },
    Rule {
        role: RoleId::Window,
        categories: &[
            "gui",
            "graphics",
            "rendering",
            "multimedia",
            "multimedia::images",
        ],
        keywords: &["gui", "ui"],
    },
    Rule {
        role: RoleId::Languages,
        categories: &["compilers", "text-editors"],
        keywords: &["tree-sitter", "compiler", "linter"],
    },
    Rule {
        role: RoleId::Hashing,
        categories: &["cryptography", "compression"],
        keywords: &[
            "hash",
            "digest",
            "crypto",
            "zip",
            "compression",
            "sha2",
            "blake3",
        ],
    },
    Rule {
        role: RoleId::Store,
        categories: &[
            "database",
            "database-implementations",
            "text-search",
            "caching",
        ],
        keywords: &["database", "sql", "sqlite", "search"],
    },
    Rule {
        role: RoleId::Network,
        categories: &[
            "web-programming",
            "web-programming::http-client",
            "web-programming::http-server",
            "web-programming::websocket",
            "network-programming",
        ],
        keywords: &["http", "https", "socket", "network"],
    },
    Rule {
        role: RoleId::Os,
        categories: &[
            "os",
            "os::unix-apis",
            "os::macos-apis",
            "os::windows-apis",
            "filesystem",
        ],
        keywords: &["syscall", "gitignore", "mmap", "memory-map"],
    },
    Rule {
        role: RoleId::Concurrency,
        categories: &["asynchronous", "concurrency"],
        keywords: &["async", "futures", "atomic", "lock-free"],
    },
    Rule {
        role: RoleId::Formats,
        categories: &["encoding", "parser-implementations", "config", "parsing"],
        keywords: &[
            "serde",
            "serialization",
            "serialize",
            "deserialize",
            "json",
            "toml",
            "xml",
            "base64",
            "binary",
            "encode",
            "decode",
            "encoding",
        ],
    },
    Rule {
        role: RoleId::Memory,
        categories: &["data-structures", "memory-management"],
        keywords: &["vec", "vector", "stack", "allocator"],
    },
];

/// Description phrases, checked after the vote, in this order.
const PHRASES: &[(RoleId, &[&str])] = &[
    (RoleId::Hashing, &["hash function", "compression"]),
    (
        RoleId::Store,
        &["sqlite", "database", "search engine", "query engine"],
    ),
    (RoleId::Observe, &["telemetry", "tracing"]),
    (RoleId::Concurrency, &["futures", "async"]),
];

/// What one dependency says about itself.
pub(super) struct Declared<'a> {
    pub categories: &'a [String],
    pub keywords: &'a [String],
    pub description: Option<&'a str>,
    pub dev_only: bool,
}

/// The role a dependency's own categories and keywords vote for (or its
/// being only a dev-dependency).
pub(super) fn declared_role(declared: &Declared<'_>) -> Option<(RoleId, RoleEvidence)> {
    if declared.dev_only {
        return Some((RoleId::Tests, RoleEvidence::DevOnly));
    }
    let categories = declared
        .categories
        .iter()
        .map(|word| word.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let keywords = declared
        .keywords
        .iter()
        .map(|word| word.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut best: Option<(usize, usize, Vec<String>)> = None;
    for (order, rule) in RULES.iter().enumerate() {
        let words = categories
            .iter()
            .filter(|word| rule.categories.contains(&word.as_str()))
            .chain(
                keywords
                    .iter()
                    .filter(|word| rule.keywords.contains(&word.as_str())),
            )
            .cloned()
            .fold(Vec::new(), |mut words, word| {
                if !words.contains(&word) {
                    words.push(word);
                }
                words
            });
        if words.is_empty() {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(count, _, _)| words.len() > *count)
        {
            best = Some((words.len(), order, words));
        }
    }
    best.map(|(_, order, words)| {
        (
            RULES[order].role,
            RoleEvidence::Declared {
                words: words.into_boxed_slice(),
            },
        )
    })
}

/// The role a telling phrase in the description gives a silent dependency.
pub(super) fn described_role(declared: &Declared<'_>) -> Option<(RoleId, RoleEvidence)> {
    let description = declared.description?.to_ascii_lowercase();
    for (role, phrases) in PHRASES {
        for phrase in *phrases {
            if contains_word(&description, phrase) {
                return Some((
                    *role,
                    RoleEvidence::Described {
                        phrase: (*phrase).to_owned(),
                    },
                ));
            }
        }
    }
    None
}

/// Whether `phrase` occurs in `text` bounded by non-alphanumerics.
fn contains_word(text: &str, phrase: &str) -> bool {
    text.match_indices(phrase).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + phrase.len()..].chars().next();
        before.is_none_or(|character| !character.is_alphanumeric())
            && after.is_none_or(|character| !character.is_alphanumeric())
    })
}

/// Chooses a role for a silent dependency from its declared peers: the
/// majority role among them, a tie going to the role shown first; the named
/// peer is the first, by name, with that role.
pub(super) fn cohort_role<'a>(
    peers: impl Iterator<Item = (&'a str, RoleId)>,
) -> Option<(RoleId, String)> {
    let mut votes: BTreeMap<RoleId, Vec<&str>> = BTreeMap::new();
    for (name, role) in peers {
        votes.entry(role).or_default().push(name);
    }
    let (role, names) = votes
        .into_iter()
        .max_by(|(left_role, left), (right_role, right)| {
            left.len()
                .cmp(&right.len())
                .then_with(|| right_role.cmp(left_role))
        })?;
    let peer = names.into_iter().min()?.to_owned();
    Some((role, peer))
}
