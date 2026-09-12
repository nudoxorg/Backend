//! Defines card behavior for `interface-mcp`, whose purpose is to serve the one shared local library to agents over MCP.
//! This module owns the card invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! `nudox://schema-card`: everything an agent must know once, and nothing it can learn from a row.
//!
//! The card is read once per session instead of being repeated in twelve tool descriptions. Its
//! prose is a constant; its relation table is generated from [`interface_search::LINK_KINDS`] and
//! [`interface_search::relation_label`], so the card cannot claim a link kind the engine does not
//! traverse or spell one differently. Its worked examples are extracted by `tests/schema_card.rs`
//! and pushed through the real argument decoders, so an example that stops decoding fails a test
//! rather than misleading a reader.

use core::fmt::Write as _;

use compiler_ir::LinkKind;
use interface_documents::Direction;
use interface_identity::ALL_ECOSYSTEMS;
use interface_search::{
    DEFAULT_RESULT_LIMIT, LINK_KINDS, MAX_GRAPH_DEPTH, MAX_RESULT_LIMIT, relation_label,
};

/// URI the card is published at.
pub const SCHEMA_CARD_URI: &str = "nudox://schema-card";

/// The question one outgoing relation answers, in the words an agent would use to ask it.
const fn outgoing_question(kind: LinkKind) -> &'static str {
    match kind {
        LinkKind::Calls => "which functions does this one call",
        LinkKind::MethodCall => "which methods does this one invoke on a receiver",
        LinkKind::TypeReference => "which types does this declaration mention",
        LinkKind::Reads => "which storage does this read",
        LinkKind::Writes => "which storage does this write",
        LinkKind::Imports => "what does this module bring into scope",
        LinkKind::Implements => "which traits does this type implement",
        LinkKind::Overrides => "which declaration does this one override",
        LinkKind::Reexports => "what does this re-export",
        LinkKind::Inherits => "which type does this inherit from",
        LinkKind::Documents => "what does this documentation point at",
    }
}

/// The question the incoming direction answers.
const fn incoming_question(kind: LinkKind) -> &'static str {
    match kind {
        LinkKind::Calls => "who calls this",
        LinkKind::MethodCall => "who invokes this method",
        LinkKind::TypeReference => "who mentions this type",
        LinkKind::Reads => "who reads this",
        LinkKind::Writes => "who writes this",
        LinkKind::Imports => "who imports this",
        LinkKind::Implements => "who implements this trait",
        LinkKind::Overrides => "who overrides this",
        LinkKind::Reexports => "who re-exports this",
        LinkKind::Inherits => "who inherits from this",
        LinkKind::Documents => "what documentation points here",
    }
}

const PROLOGUE: &str = r#"# nudox schema card

Read this once. Nothing below changes during a session, and no row of any tool result repeats it.

## Addresses

```text
cargo:serde@1.0.196::de::Deserializer[trait]::deserialize_map[fn]#<key>
└─── coordinate ───┘└──────────── path (kind tags optional) ─────────┘ └key┘
```

* An address is `ecosystem:name@version` then `::`-separated declaration names, each optionally
  qualified by `[fn]`, `[struct]`, `[trait]`, `[mod]`, `[enum]`, `[impl]`, `[type]`, `[const]`,
  `[static]`, `[macro]`, `[use]`, `[field]`, `[variant]`, `[param]`, `[ns]`.
* Every address a result prints is resolvable. Pass it back **verbatim**, including the kind tags.
* The `~1c4b7e02` beside a row is an eight-character **fingerprint for your eyes**. It is never accepted as input. The one full key on a page appears in that page's meta line.
* An address is spelled relative to the page you are reading. A bare path belongs to the page's own
  package. `cargo:serde › de::Error[trait]` names another loaded package; hand `cargo:serde::de::Error`
  to `resolve` to learn its pinned version. `⟨cargo serde⟩ de::Error` means that package
  **is not loaded here** — never that the declaration does not exist.

## Signals

Lines beginning with `~` are signals, not prose.

* `~lanes exact✓2 names◐5 3/7 graph✗ no-index semantic✗ no-embedder` — one entry per lane.
  `✓` ran completely, `◐` ran partially (searched/total packages) or with a named degradation,
  `✗` did not run and says why. The number after the glyph is that lane's row count.
  **A lane that could not run also returns zero rows. Read this line before concluding nothing exists.**
* `~scope` — narrowing that was applied but not asked for.
* `~graph` — coverage of one relation traversal.
* `~index` — how much of the local registry index `index-search` actually searched. It answers from
  durable projections without running a compiler, so it only knows what this machine's index has
  observed; an empty result with `~index ✗` never means the package does not exist. The index knows
  name, version, checksum, yanked state, and nothing else — no descriptions, no dependencies.

## Faults

```text
✗ package-not-on-shelf cargo:serde@1.0.196
  no shelf row names this coordinate; absent here never means absent everywhere
  → add {"package":"pkg:cargo/serde@1.0.196"}
```

The `→` line is the exact next call, already filled in. `no action available` means this layer has
nothing honest to offer.
"#;

const EPILOGUE: &str = r#"
## Confidence

Relation and search rows print a confidence word only when it is weaker than compiler-proved, so an
unlabelled row is the strong case. Weakest to strongest: `syntactic` (a name matched in source),
`heuristic` (inferred), `indexed` (from a durable projection), `imported` (proved across a package
boundary), and compiler-proved, which prints nothing.

## Worked calls

Each block is a real `tools/call` payload. They are extracted from this card by a test and pushed
through the same decoders the server uses, so they cannot drift from what the server accepts.

```json
{"tool":"search","arguments":{"query":"deserialize map","kinds":["fn","trait"],"limit":10}}
```

```json
{"tool":"show","arguments":{"address":"cargo:serde@1.0.196::de::Deserializer[trait]::deserialize_map[fn]"}}
```

```json
{"tool":"graph","arguments":{"address":"cargo:serde@1.0.196::de::Deserializer[trait]","relation":"implemented-by","depth":1,"limit":50}}
```
"#;

/// Renders the card, generating every table from the shared vocabulary it describes.
#[must_use]
pub fn render() -> String {
    let mut card = String::with_capacity(6 * 1024);
    card.push_str(PROLOGUE);
    card.push_str("\n## Ecosystems\n\nA coordinate begins with exactly one of: ");
    let tags: Vec<&str> = ALL_ECOSYSTEMS
        .into_iter()
        .map(|ecosystem| interface_identity::ecosystem_tag(ecosystem).as_str())
        .collect();
    let _ = writeln!(card, "{}.\n", tags.join(" "));
    card.push_str("## Relations\n\n");
    let _ = writeln!(
        card,
        "`graph` takes one `relation` spelling, which carries its own direction. Depth is 1 to {MAX_GRAPH_DEPTH}; \
limit is 1 to {MAX_RESULT_LIMIT}, defaulting to {DEFAULT_RESULT_LIMIT}. Omitting `relation` traverses every kind.\n"
    );
    for kind in LINK_KINDS {
        let _ = writeln!(
            card,
            "- `{}` — {} · `{}` — {}",
            relation_label(kind, Direction::Outgoing),
            outgoing_question(kind),
            relation_label(kind, Direction::Incoming),
            incoming_question(kind)
        );
    }
    card.push_str(EPILOGUE);
    card
}

/// Extracts every fenced `json` block from the card, in order.
///
/// Shared with `tests/schema_card.rs` so the test cannot extract examples differently from anything
/// else that reads them.
#[must_use]
pub fn worked_examples(card: &str) -> Vec<&str> {
    let mut examples = Vec::new();
    let mut rest = card;
    while let Some(open) = rest.find("```json\n") {
        let Some(body) = rest.get(open.saturating_add(8)..) else {
            break;
        };
        let Some(close) = body.find("\n```") else {
            break;
        };
        if let Some(example) = body.get(..close) {
            examples.push(example);
        }
        rest = body.get(close.saturating_add(4)..).unwrap_or("");
    }
    examples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_link_kind_appears_in_both_directions() {
        let card = render();
        for kind in LINK_KINDS {
            for direction in [Direction::Outgoing, Direction::Incoming] {
                let label = relation_label(kind, direction);
                assert!(
                    card.contains(&format!("`{label}`")),
                    "the card never spells the relation {label}"
                );
            }
        }
    }

    #[test]
    fn the_card_carries_three_worked_examples() {
        let card = render();
        assert_eq!(worked_examples(&card).len(), 3);
    }
}
