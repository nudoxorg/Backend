//! Snapshot harness for the Go producer.
//!
//! # Why JSON fixtures, not the real oracle?
//!
//! Invoking the oracle requires a compiled `nudox-go-oracle` binary in `PATH`
//! (or `$NUDOX_GO_ORACLE_BIN`) which is only available inside Buck2 sandboxes.
//! Under plain `cargo test` the binary is absent and calling
//! `GoProducer::produce` would immediately return `GoError::Oracle`.
//!
//! Instead we embed hand-authored JSON fixtures that closely mirror what the
//! real oracle would emit for the committed Go source files under
//! `workspace/compiler/tests/fixtures/go/`.  The projection is deliberately
//! source-aligned: every type, method, and constant named in those `.go` files
//! appears in the snapshot.
//!
//! # Projection design
//!
//! We snapshot a **stable, sorted projection** of `IrPackage::iter()`.
//! Fields chosen and why:
//!
//! - `kind` — the IR discriminant (Module/Record/Function/Enum/…).  Catches any
//!   misclassification (e.g. interface becoming Record, iota enum becoming Const).
//! - `name` — the entry's own name.  Catches dropped or renamed symbols.
//! - `visibility` — Public vs Package/Private.  Catches export-bit regressions.
//! - `doc_present` — bool.  Catches silently dropped documentation.
//! - `deprecation_present` — bool.  Catches lost `Deprecated:` paragraphs.
//! - `generic_count` — number of type params.  Catches dropped generics.
//! - `alias_count` — number of search aliases.  The primary regression gate for
//!   Part 1: a method that loses its aliases shows `alias_count: 0` here.
//! - `child_count` — number of direct IR children (fields, methods, params).
//!   Catches structural losses such as a struct losing its fields.
//! - `receiver` — for Function entries, the receiver kind.  Catches
//!   value-vs-pointer confusion and interface-method vs method distinction.
//!
//! Param entries are excluded from the projection: they are structural children
//! of Function entries and their counts are captured by `child_count`.  Params
//! are numerous and their content varies per signature, making them noisy
//! without adding much regression signal above what `child_count` already gives.
//!
//! All lists are sorted lexicographically before snapshotting so that HashMap
//! and BTreeMap iteration order never reaches the output.

use nudox_ir::{
    change::{EcosystemId, PackageLineageId, PackageName},
    kinds::{Alias, Const, Enum, Field, Function, Module, Param, Record, Static, Trait, Variant},
    package::PackageId,
};

use nudox_producer_go::producer::GoProducer;

// ---------------------------------------------------------------------------
// Projection types
// ---------------------------------------------------------------------------

/// A deterministic, human-readable projection of one IR entry.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EntryRow {
    /// Stable sort key: kind discriminant name then symbol name.
    /// Stored first so `derive(Ord)` sorts by kind, then name.
    kind: String,
    name: String,
    visibility: String,
    doc_present: bool,
    deprecation_present: bool,
    generic_count: usize,
    alias_count: usize,
    child_count: usize,
    /// For Function entries: `Some("Owned")`, `Some("MutRef")`, `Some("Arbitrary")`
    /// or `None` for free functions and interface requirements.
    /// `None` for all other kinds.
    receiver: Option<String>,
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("go"), PackageName::new(name))
}

/// Classify an entry into a stable kind-name string using public `downcast` API.
///
/// Returns `None` for Param entries (excluded from projection) and Reference
/// entries (re-exports, rare in Go output).
fn classify(entry: &nudox_ir::entry::Entry) -> Option<&'static str> {
    if entry.downcast::<Param>().is_some() {
        return None; // excluded — captured by parent's child_count
    }
    if entry.downcast::<Module>().is_some() {
        return Some("Module");
    }
    if entry.downcast::<Record>().is_some() {
        return Some("Record");
    }
    if entry.downcast::<Field>().is_some() {
        return Some("Field");
    }
    if entry.downcast::<Function>().is_some() {
        return Some("Function");
    }
    if entry.downcast::<Alias>().is_some() {
        return Some("Alias");
    }
    if entry.downcast::<Trait>().is_some() {
        return Some("Trait");
    }
    if entry.downcast::<Enum>().is_some() {
        return Some("Enum");
    }
    if entry.downcast::<Variant>().is_some() {
        return Some("Variant");
    }
    if entry.downcast::<Const>().is_some() {
        return Some("Const");
    }
    if entry.downcast::<Static>().is_some() {
        return Some("Static");
    }
    // Reference entries (re-exports) — skip; they are rare in Go output.
    None
}

/// Lower raw oracle JSON and project the IR into a sorted Vec of `EntryRow`.
///
/// Param entries are excluded (structural children of functions; captured by
/// child_count).
fn project(oracle_json: &str, pkg_id: &str, lineage_name: &str) -> Vec<EntryRow> {
    let producer = GoProducer;
    let pkg = producer
        .lower_bytes(
            oracle_json.as_bytes(),
            PackageId::path(pkg_id),
            &lineage(lineage_name),
        )
        .expect("fixture must lower without error");

    let mut rows: Vec<EntryRow> = pkg
        .iter()
        .filter_map(|(_, entry)| {
            let kind_name = classify(entry)?;

            let sym = entry.sym();

            let generic_count = entry
                .downcast::<Function>()
                .map(|f| f.body().generics.len())
                .or_else(|| entry.downcast::<Record>().map(|r| r.body().generics.len()))
                .or_else(|| entry.downcast::<Trait>().map(|t| t.body().generics.len()))
                .unwrap_or(0);

            let receiver = entry
                .downcast::<Function>()
                .map(|f| match f.body().receiver {
                    Some(r) => format!("{r:?}"),
                    None => "None".to_string(),
                });

            Some(EntryRow {
                kind: kind_name.to_string(),
                name: sym.name.clone(),
                visibility: format!("{:?}", sym.visibility),
                doc_present: !sym.documentation.is_empty(),
                deprecation_present: sym.deprecation.is_some(),
                generic_count,
                alias_count: sym.aliases.len(),
                child_count: entry.children().len(),
                receiver,
            })
        })
        .collect();

    rows.sort();
    rows
}

// ---------------------------------------------------------------------------
// Fixture JSON: "basics"
//
// Mirrors workspace/compiler/tests/fixtures/go/basics/types.go.
// Covers: struct with fields+tags+embedded, methods (pointer + value receiver),
// iota enum, newtype, true alias, named types (slice/map/chan/ptr), const,
// var, package-level func (multi-param, multi-return, variadic), named returns.
// ---------------------------------------------------------------------------

const BASICS_JSON: &str = r#"
{
  "module": { "path": "example.com/basics", "dir": "/tmp/basics", "goVersion": "1.22" },
  "packages": [
    {
      "importPath": "example.com/basics",
      "name": "basics",
      "doc": "Package basics exercises exported vs unexported identifiers, struct fields, named types, type aliases, slices, maps, channels, and pointers.",
      "decls": [
        {
          "kind": "type",
          "name": "Server",
          "exported": true,
          "doc": "Server is an exported struct with mixed exported/unexported fields and struct tags.",
          "underlying": {
            "kind": "struct",
            "fields": [
              { "name": "Addr",    "exported": true,  "tag": "json:\"addr\"", "embedded": false, "type": { "kind": "basic", "name": "string" } },
              { "name": "Port",    "exported": true,  "tag": "json:\"port\"", "embedded": false, "type": { "kind": "basic", "name": "int" } },
              { "name": "timeout", "exported": false, "tag": "",              "embedded": false,
                "type": { "kind": "basic", "name": "int64" } }
            ]
          },
          "methods": [
            {
              "name": "Close",
              "exported": true,
              "doc": "Close shuts down the server with a pointer receiver.",
              "pointerRecv": true,
              "signature": {
                "kind": "func",
                "results": [{ "name": "", "type": { "kind": "basic", "name": "error" } }]
              }
            },
            {
              "name": "Status",
              "exported": true,
              "doc": "Status returns the current status string with a value receiver.",
              "pointerRecv": false,
              "signature": {
                "kind": "func",
                "results": [{ "name": "", "type": { "kind": "basic", "name": "string" } }]
              }
            }
          ],
          "fieldDocs": {
            "Addr": "Addr is the listen address.",
            "Port": "Port is the port number."
          }
        },
        {
          "kind": "type",
          "name": "Config",
          "exported": true,
          "doc": "Config is an exported struct that embeds Server.",
          "underlying": {
            "kind": "struct",
            "fields": [
              { "name": "Server", "exported": true, "tag": "", "embedded": true,
                "type": { "kind": "named", "pkg": "example.com/basics", "name": "Server" } },
              { "name": "Name",   "exported": true, "tag": "json:\"name\"", "embedded": false,
                "type": { "kind": "basic", "name": "string" } }
            ]
          },
          "promotedMethods": [
            {
              "name": "Close",
              "exported": true,
              "doc": "Close shuts down the server with a pointer receiver.",
              "pointerRecv": true,
              "origin": "example.com/basics.Server",
              "signature": {
                "kind": "func",
                "results": [{ "name": "", "type": { "kind": "basic", "name": "error" } }]
              }
            },
            {
              "name": "Status",
              "exported": true,
              "doc": "Status returns the current status string with a value receiver.",
              "pointerRecv": false,
              "origin": "example.com/basics.Server",
              "signature": {
                "kind": "func",
                "results": [{ "name": "", "type": { "kind": "basic", "name": "string" } }]
              }
            }
          ],
          "fieldDocs": { "Name": "Name identifies the configuration." }
        },
        {
          "kind": "type",
          "name": "Direction",
          "exported": true,
          "doc": "Direction is an iota-based enum (Go convention: defined type + iota consts).",
          "underlying": { "kind": "basic", "name": "int" }
        },
        {
          "kind": "const", "name": "North", "exported": true, "doc": "",
          "type": { "kind": "named", "pkg": "example.com/basics", "name": "Direction" },
          "value": "0", "constGroup": 1, "groupHasIota": true
        },
        {
          "kind": "const", "name": "East", "exported": true, "doc": "",
          "type": { "kind": "named", "pkg": "example.com/basics", "name": "Direction" },
          "value": "1", "constGroup": 1, "groupHasIota": true
        },
        {
          "kind": "const", "name": "South", "exported": true, "doc": "",
          "type": { "kind": "named", "pkg": "example.com/basics", "name": "Direction" },
          "value": "2", "constGroup": 1, "groupHasIota": true
        },
        {
          "kind": "const", "name": "West", "exported": true, "doc": "",
          "type": { "kind": "named", "pkg": "example.com/basics", "name": "Direction" },
          "value": "3", "constGroup": 1, "groupHasIota": true
        },
        {
          "kind": "type",
          "name": "Celsius",
          "exported": true,
          "doc": "Celsius is a named (defined) type over float64 (becomes a newtype RecordType).",
          "underlying": { "kind": "basic", "name": "float64" }
        },
        {
          "kind": "alias",
          "name": "Temperature",
          "exported": true,
          "doc": "Temperature is a true type alias.",
          "target": { "kind": "named", "pkg": "example.com/basics", "name": "Celsius" }
        },
        {
          "kind": "type",
          "name": "StringSlice",
          "exported": true,
          "doc": "StringSlice is a named type over a slice.",
          "underlying": { "kind": "slice", "elem": { "kind": "basic", "name": "string" } }
        },
        {
          "kind": "type",
          "name": "StringMap",
          "exported": true,
          "doc": "StringMap is a named type over a map.",
          "underlying": {
            "kind": "map",
            "key":  { "kind": "basic", "name": "string" },
            "value": { "kind": "basic", "name": "string" }
          }
        },
        {
          "kind": "type",
          "name": "Chan",
          "exported": true,
          "doc": "Chan is a named type over a channel.",
          "underlying": { "kind": "chan", "dir": "both", "elem": { "kind": "basic", "name": "string" } }
        },
        {
          "kind": "type",
          "name": "Ptr",
          "exported": true,
          "doc": "Ptr is a named type over a pointer.",
          "underlying": { "kind": "pointer", "base": { "kind": "named", "pkg": "example.com/basics", "name": "Server" } }
        },
        {
          "kind": "const",
          "name": "MaxRetries",
          "exported": true,
          "doc": "MaxRetries is an exported constant.",
          "type": { "kind": "basic", "name": "int" },
          "value": "3",
          "constGroup": 2,
          "groupHasIota": false
        },
        {
          "kind": "const",
          "name": "defaultTimeout",
          "exported": false,
          "doc": "defaultTimeout is an unexported constant.",
          "type": { "kind": "basic", "name": "int" },
          "value": "30",
          "constGroup": 3,
          "groupHasIota": false
        },
        {
          "kind": "var",
          "name": "Count",
          "exported": true,
          "doc": "Count is an exported package-level variable.",
          "type": { "kind": "basic", "name": "int" }
        },
        {
          "kind": "func",
          "name": "Process",
          "exported": true,
          "doc": "Process is a top-level exported function: multiple params, multiple returns.\n\nIt accepts a name and a count and returns the processed string and an error.",
          "signature": {
            "kind": "func",
            "params": [
              { "name": "name",  "type": { "kind": "basic", "name": "string" } },
              { "name": "count", "type": { "kind": "basic", "name": "int" } }
            ],
            "results": [
              { "name": "", "type": { "kind": "basic", "name": "string" } },
              { "name": "", "type": { "kind": "basic", "name": "error" } }
            ]
          }
        },
        {
          "kind": "func",
          "name": "Variadic",
          "exported": true,
          "doc": "Variadic accepts a variadic argument.",
          "signature": {
            "kind": "func",
            "variadic": true,
            "params": [
              { "name": "prefix", "type": { "kind": "basic", "name": "string" } },
              { "name": "items",  "type": { "kind": "basic", "name": "string" } }
            ],
            "results": [
              { "name": "", "type": { "kind": "slice", "elem": { "kind": "basic", "name": "string" } } }
            ]
          }
        },
        {
          "kind": "func",
          "name": "namedReturn",
          "exported": false,
          "doc": "namedReturn demonstrates named return values (unexported, for coverage).",
          "signature": {
            "kind": "func",
            "params": [
              { "name": "n", "type": { "kind": "basic", "name": "int" } }
            ],
            "results": [
              { "name": "result", "type": { "kind": "basic", "name": "string" } },
              { "name": "err",    "type": { "kind": "basic", "name": "error" } }
            ]
          }
        }
      ]
    }
  ]
}
"#;

// ---------------------------------------------------------------------------
// Fixture JSON: "interfaces"
//
// Mirrors workspace/compiler/tests/fixtures/go/interfaces/ifaces.go.
// Covers: simple interfaces, embedded interfaces, generic interface+constraint,
// generic struct with methods, generic top-level func, named map type,
// unexported function.
// ---------------------------------------------------------------------------

const INTERFACES_JSON: &str = r#"
{
  "module": { "path": "example.com/interfaces", "dir": "/tmp/interfaces", "goVersion": "1.22" },
  "packages": [
    {
      "importPath": "example.com/interfaces",
      "name": "interfaces",
      "doc": "Package interfaces exercises Go interface declarations, embedded interfaces, generics, and constraint type sets.",
      "decls": [
        {
          "kind": "type",
          "name": "Reader",
          "exported": true,
          "doc": "Reader is a simple exported interface.\n\nIt has one required method with a single parameter.",
          "underlying": {
            "kind": "interface",
            "explicitMethods": [
              {
                "name": "Read",
                "exported": true,
                "doc": "Read fills p and returns the count and any error.",
                "signature": {
                  "kind": "func",
                  "params": [{ "name": "p", "type": { "kind": "slice", "elem": { "kind": "basic", "name": "uint8" } } }],
                  "results": [
                    { "name": "n",   "type": { "kind": "basic", "name": "int" } },
                    { "name": "err", "type": { "kind": "basic", "name": "error" } }
                  ]
                }
              }
            ],
            "allMethods": [
              {
                "name": "Read",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "params": [{ "name": "p", "type": { "kind": "slice", "elem": { "kind": "basic", "name": "uint8" } } }],
                  "results": [
                    { "name": "n",   "type": { "kind": "basic", "name": "int" } },
                    { "name": "err", "type": { "kind": "basic", "name": "error" } }
                  ]
                }
              }
            ]
          }
        },
        {
          "kind": "type",
          "name": "Writer",
          "exported": true,
          "doc": "Writer is a complementary interface.",
          "underlying": {
            "kind": "interface",
            "explicitMethods": [
              {
                "name": "Write",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "params": [{ "name": "p", "type": { "kind": "slice", "elem": { "kind": "basic", "name": "uint8" } } }],
                  "results": [
                    { "name": "n",   "type": { "kind": "basic", "name": "int" } },
                    { "name": "err", "type": { "kind": "basic", "name": "error" } }
                  ]
                }
              }
            ],
            "allMethods": [
              {
                "name": "Write",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "params": [{ "name": "p", "type": { "kind": "slice", "elem": { "kind": "basic", "name": "uint8" } } }],
                  "results": [
                    { "name": "n",   "type": { "kind": "basic", "name": "int" } },
                    { "name": "err", "type": { "kind": "basic", "name": "error" } }
                  ]
                }
              }
            ]
          }
        },
        {
          "kind": "type",
          "name": "ReadWriter",
          "exported": true,
          "doc": "ReadWriter embeds both Reader and Writer (method-set union via embedding).",
          "underlying": {
            "kind": "interface",
            "explicitMethods": [],
            "allMethods": [
              {
                "name": "Read",
                "exported": true,
                "pkg": "example.com/interfaces",
                "signature": {
                  "kind": "func",
                  "params": [{ "name": "p", "type": { "kind": "slice", "elem": { "kind": "basic", "name": "uint8" } } }],
                  "results": [
                    { "name": "n",   "type": { "kind": "basic", "name": "int" } },
                    { "name": "err", "type": { "kind": "basic", "name": "error" } }
                  ]
                }
              },
              {
                "name": "Write",
                "exported": true,
                "pkg": "example.com/interfaces",
                "signature": {
                  "kind": "func",
                  "params": [{ "name": "p", "type": { "kind": "slice", "elem": { "kind": "basic", "name": "uint8" } } }],
                  "results": [
                    { "name": "n",   "type": { "kind": "basic", "name": "int" } },
                    { "name": "err", "type": { "kind": "basic", "name": "error" } }
                  ]
                }
              }
            ]
          }
        },
        {
          "kind": "type",
          "name": "Closer",
          "exported": true,
          "doc": "Closer closes a resource.",
          "underlying": {
            "kind": "interface",
            "explicitMethods": [
              {
                "name": "Close",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "results": [{ "name": "", "type": { "kind": "basic", "name": "error" } }]
                }
              }
            ],
            "allMethods": [
              {
                "name": "Close",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "results": [{ "name": "", "type": { "kind": "basic", "name": "error" } }]
                }
              }
            ]
          }
        },
        {
          "kind": "type",
          "name": "ReadWriteCloser",
          "exported": true,
          "doc": "ReadWriteCloser embeds all three.",
          "underlying": {
            "kind": "interface",
            "explicitMethods": [],
            "allMethods": [
              {
                "name": "Close",
                "exported": true,
                "pkg": "example.com/interfaces",
                "signature": {
                  "kind": "func",
                  "results": [{ "name": "", "type": { "kind": "basic", "name": "error" } }]
                }
              },
              {
                "name": "Read",
                "exported": true,
                "pkg": "example.com/interfaces",
                "signature": {
                  "kind": "func",
                  "params": [{ "name": "p", "type": { "kind": "slice", "elem": { "kind": "basic", "name": "uint8" } } }],
                  "results": [
                    { "name": "n",   "type": { "kind": "basic", "name": "int" } },
                    { "name": "err", "type": { "kind": "basic", "name": "error" } }
                  ]
                }
              },
              {
                "name": "Write",
                "exported": true,
                "pkg": "example.com/interfaces",
                "signature": {
                  "kind": "func",
                  "params": [{ "name": "p", "type": { "kind": "slice", "elem": { "kind": "basic", "name": "uint8" } } }],
                  "results": [
                    { "name": "n",   "type": { "kind": "basic", "name": "int" } },
                    { "name": "err", "type": { "kind": "basic", "name": "error" } }
                  ]
                }
              }
            ]
          }
        },
        {
          "kind": "type",
          "name": "Ordered",
          "exported": true,
          "doc": "Ordered is a constraint type set: integers, floats, and strings.",
          "underlying": {
            "kind": "interface",
            "explicitMethods": [],
            "allMethods": []
          }
        },
        {
          "kind": "type",
          "name": "Container",
          "exported": true,
          "doc": "Container is a generic interface constrained to ordered types (Go 1.18+).\n\nT must satisfy the Ordered constraint.",
          "typeParams": [
            { "name": "T", "constraint": { "kind": "named", "pkg": "example.com/interfaces", "name": "Ordered" } }
          ],
          "underlying": {
            "kind": "interface",
            "explicitMethods": [
              {
                "name": "Add",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "params": [{ "name": "item", "type": { "kind": "typeParam", "name": "T" } }]
                }
              },
              {
                "name": "Len",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "results": [{ "name": "", "type": { "kind": "basic", "name": "int" } }]
                }
              },
              {
                "name": "Get",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "params": [{ "name": "i", "type": { "kind": "basic", "name": "int" } }],
                  "results": [{ "name": "", "type": { "kind": "typeParam", "name": "T" } }]
                }
              }
            ],
            "allMethods": [
              {
                "name": "Add", "exported": true,
                "signature": { "kind": "func", "params": [{ "name": "item", "type": { "kind": "typeParam", "name": "T" } }] }
              },
              {
                "name": "Get", "exported": true,
                "signature": {
                  "kind": "func",
                  "params":  [{ "name": "i", "type": { "kind": "basic", "name": "int" } }],
                  "results": [{ "name": "", "type": { "kind": "typeParam", "name": "T" } }]
                }
              },
              {
                "name": "Len", "exported": true,
                "signature": { "kind": "func", "results": [{ "name": "", "type": { "kind": "basic", "name": "int" } }] }
              }
            ]
          }
        },
        {
          "kind": "type",
          "name": "Stack",
          "exported": true,
          "doc": "Stack is a generic struct implementing a container pattern.\n\nItems are stored in a slice and the zero value is ready to use.",
          "typeParams": [
            { "name": "T", "constraint": { "kind": "interface" } }
          ],
          "underlying": {
            "kind": "struct",
            "fields": [
              { "name": "items", "exported": false,
                "type": { "kind": "slice", "elem": { "kind": "typeParam", "name": "T" } } }
            ]
          },
          "methods": [
            {
              "name": "Push",
              "exported": true,
              "doc": "Push adds an item to the top of the stack with a pointer receiver.",
              "pointerRecv": true,
              "signature": {
                "kind": "func",
                "params": [{ "name": "item", "type": { "kind": "typeParam", "name": "T" } }]
              }
            },
            {
              "name": "Pop",
              "exported": true,
              "doc": "Pop removes and returns the top item, and whether it was present.",
              "pointerRecv": true,
              "signature": {
                "kind": "func",
                "results": [
                  { "name": "", "type": { "kind": "typeParam", "name": "T" } },
                  { "name": "", "type": { "kind": "basic", "name": "bool" } }
                ]
              }
            }
          ]
        },
        {
          "kind": "func",
          "name": "Transform",
          "exported": true,
          "doc": "Transform applies a function to every item in a slice and returns results.\n\nThis exercises a top-level generic function with multiple type parameters.",
          "typeParams": [
            { "name": "In",  "constraint": { "kind": "interface" } },
            { "name": "Out", "constraint": { "kind": "interface" } }
          ],
          "signature": {
            "kind": "func",
            "params": [
              { "name": "items", "type": { "kind": "slice", "elem": { "kind": "typeParam", "name": "In" } } },
              { "name": "fn",    "type": { "kind": "func",
                "params":  [{ "name": "", "type": { "kind": "typeParam", "name": "In" } }],
                "results": [{ "name": "", "type": { "kind": "typeParam", "name": "Out" } }]
              }}
            ],
            "results": [
              { "name": "", "type": { "kind": "slice", "elem": { "kind": "typeParam", "name": "Out" } } }
            ]
          }
        },
        {
          "kind": "type",
          "name": "Map",
          "exported": true,
          "doc": "Map is a named type over a generic map, demonstrating type params on a named type.",
          "typeParams": [
            { "name": "K", "constraint": { "kind": "interface", "isComparable": true } },
            { "name": "V", "constraint": { "kind": "interface" } }
          ],
          "underlying": {
            "kind": "map",
            "key":   { "kind": "typeParam", "name": "K" },
            "value": { "kind": "typeParam", "name": "V" }
          }
        },
        {
          "kind": "func",
          "name": "unexportedHelper",
          "exported": false,
          "doc": "unexportedHelper is private and should lower with unexported visibility.",
          "signature": {
            "kind": "func",
            "params":  [{ "name": "s", "type": { "kind": "basic", "name": "string" } }],
            "results": [{ "name": "", "type": { "kind": "basic", "name": "string" } }]
          }
        }
      ]
    }
  ]
}
"#;

// ---------------------------------------------------------------------------
// Fixture JSON: "multipackage"
//
// Mirrors workspace/compiler/tests/fixtures/go/multipackage/ (two packages).
// Covers: multi-package module, pointer return type, multi-param same type,
// methods (pointer receiver), sub-package.
// ---------------------------------------------------------------------------

const MULTIPACKAGE_JSON: &str = r#"
{
  "module": { "path": "example.com/multipackage", "dir": "/tmp/multipackage", "goVersion": "1.22" },
  "packages": [
    {
      "importPath": "example.com/multipackage",
      "name": "multipackage",
      "doc": "Package multipackage is the root package of a multi-package module.",
      "decls": [
        {
          "kind": "type",
          "name": "Registry",
          "exported": true,
          "doc": "Registry holds key-value entries.",
          "underlying": {
            "kind": "struct",
            "fields": [
              { "name": "entries", "exported": false,
                "type": { "kind": "map",
                  "key":   { "kind": "basic", "name": "string" },
                  "value": { "kind": "basic", "name": "string" }
                }
              }
            ]
          },
          "methods": [
            {
              "name": "Set",
              "exported": true,
              "doc": "Set stores a value under key.",
              "pointerRecv": true,
              "signature": {
                "kind": "func",
                "params": [
                  { "name": "key",   "type": { "kind": "basic", "name": "string" } },
                  { "name": "value", "type": { "kind": "basic", "name": "string" } }
                ]
              }
            },
            {
              "name": "Get",
              "exported": true,
              "doc": "Get retrieves the value for key.",
              "pointerRecv": true,
              "signature": {
                "kind": "func",
                "params": [
                  { "name": "key", "type": { "kind": "basic", "name": "string" } }
                ],
                "results": [
                  { "name": "",   "type": { "kind": "basic", "name": "string" } },
                  { "name": "ok", "type": { "kind": "basic", "name": "bool" } }
                ]
              }
            }
          ]
        },
        {
          "kind": "func",
          "name": "NewRegistry",
          "exported": true,
          "doc": "NewRegistry creates an empty registry.",
          "signature": {
            "kind": "func",
            "results": [
              { "name": "", "type": { "kind": "pointer", "base": { "kind": "named", "pkg": "example.com/multipackage", "name": "Registry" } } }
            ]
          }
        }
      ]
    },
    {
      "importPath": "example.com/multipackage/sub",
      "name": "sub",
      "doc": "Package sub is a sub-package within the multipackage module.\nIt demonstrates that the context assembles multi-package modules correctly.",
      "decls": [
        {
          "kind": "type",
          "name": "Helper",
          "exported": true,
          "doc": "Helper provides utility operations for the parent module.",
          "underlying": {
            "kind": "struct",
            "fields": [
              { "name": "Name", "exported": true, "type": { "kind": "basic", "name": "string" } }
            ]
          },
          "methods": [
            {
              "name": "Run",
              "exported": true,
              "doc": "Run executes the helper's primary action.",
              "pointerRecv": true,
              "signature": {
                "kind": "func",
                "results": [{ "name": "", "type": { "kind": "basic", "name": "error" } }]
              }
            },
            {
              "name": "Describe",
              "exported": true,
              "doc": "Describe returns a description string with a value receiver.",
              "pointerRecv": false,
              "signature": {
                "kind": "func",
                "results": [{ "name": "", "type": { "kind": "basic", "name": "string" } }]
              }
            }
          ],
          "fieldDocs": { "Name": "Name is the helper's identifier." }
        },
        {
          "kind": "func",
          "name": "New",
          "exported": true,
          "doc": "New constructs a Helper.",
          "signature": {
            "kind": "func",
            "params": [{ "name": "name", "type": { "kind": "basic", "name": "string" } }],
            "results": [
              { "name": "", "type": { "kind": "pointer", "base": { "kind": "named", "pkg": "example.com/multipackage/sub", "name": "Helper" } } }
            ]
          }
        }
      ]
    }
  ]
}
"#;

// ---------------------------------------------------------------------------
// Snapshot tests
// ---------------------------------------------------------------------------

#[test]
fn snap_basics_ir() {
    let rows = project(BASICS_JSON, "example.com/basics", "example.com/basics");
    insta::assert_debug_snapshot!("go_basics_ir", rows);
}

#[test]
fn snap_interfaces_ir() {
    let rows = project(
        INTERFACES_JSON,
        "example.com/interfaces",
        "example.com/interfaces",
    );
    insta::assert_debug_snapshot!("go_interfaces_ir", rows);
}

#[test]
fn snap_multipackage_ir() {
    let rows = project(
        MULTIPACKAGE_JSON,
        "example.com/multipackage",
        "example.com/multipackage",
    );
    insta::assert_debug_snapshot!("go_multipackage_ir", rows);
}
