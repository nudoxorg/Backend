# Authors every syntax lint as one evaluated Nix value.
# Couples policy metadata, the matcher, and adversarial neighbors without YAML duplication.
# Rejects incomplete lint laws before ast-grep or Nushell can execute.
let
  required = [
    "category"
    "exception"
    "message"
    "note"
    "scope"
    "severity"
  ];
  mkRustRule =
    id: specification:
    let
      missing = builtins.filter (field: !(builtins.hasAttr field specification)) required;
      valid = specification.valid or [ ];
      invalid = specification.invalid or [ ];
      snapshotCounts = specification.snapshotCounts or (map (_: 1) invalid);
    in
    assert builtins.match "^[a-z][a-z0-9-]+$" id != null;
    assert missing == [ ];
    assert builtins.elem specification.severity [
      "error"
      "warning"
    ];
    assert builtins.length valid >= 2;
    assert builtins.length invalid >= 2;
    assert builtins.length snapshotCounts == builtins.length invalid;
    specification
    // {
      inherit
        id
        invalid
        snapshotCounts
        valid
        ;
      language = "Rust";
      proof = "matrix-snapshot";
    };
in
{
  no-manual-display-impl = mkRustRule "no-manual-display-impl" {
    category = "diagnostic";
    severity = "warning";
    scope = "manual standard Display implementations";
    exception = "reviewed stable wire text or deliberately redacted foreign diagnostic";
    message = "Derive a declarative Display or Error representation instead of maintaining formatter control flow.";
    note = "Prefer thiserror or a vocabulary-owned derive; manual formatting needs an explicit stability or redaction reason.";
    matcher.any = [
      { pattern = "impl core::fmt::Display for $TYPE { $$$ITEMS }"; }
      { pattern = "impl std::fmt::Display for $TYPE { $$$ITEMS }"; }
      { pattern = "impl fmt::Display for $TYPE { $$$ITEMS }"; }
    ];
    valid = [
      ''#[derive(Debug, thiserror::Error)] #[error("missing {field}")] struct Failure { field: Field }''
      ''#[derive(Debug, derive_more::Display)] #[display("{value}")] struct Count { value: usize }''
    ];
    invalid = [
      ''impl core::fmt::Display for Failure { fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { write!(formatter, "failure") } }''
      ''impl fmt::Display for Phase { fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result { formatter.write_str("phase") } }''
    ];
  };

  no-shared-owner-field = mkRustRule "no-shared-owner-field" {
    category = "ownership";
    severity = "warning";
    scope = "Arc-bearing fields";
    exception = "measured immutable cross-thread authority with explicit sharing semantics";
    message = "Hide or remove shared ownership instead of exporting Arc as domain vocabulary.";
    note = "Public vocabulary should express authority and borrowing; an earned Arc belongs behind the owning boundary.";
    matcher.all = [
      { kind = "field_declaration"; }
      {
        has = {
          kind = "type_identifier";
          regex = "^Arc$";
          stopBy = "end";
        };
      }
    ];
    valid = [
      "pub struct Snapshot<'bytes> { pub bytes: &'bytes [u8] }"
      "pub struct OwnedSnapshot { storage: Box<[u8]> }"
    ];
    invalid = [
      "pub struct Admission { pub shared: Arc<State> }"
      "pub struct Reply { pub bytes: std::sync::Arc<[u8]> }"
    ];
  };

  no-owned-vec-field = mkRustRule "no-owned-vec-field" {
    category = "allocation";
    severity = "warning";
    scope = "Vec-bearing fields";
    exception = "explicit unbounded collection transfer object at a foreign boundary";
    message = "Expose a bounded domain collection or borrowed view instead of a public Vec.";
    note = "The owning type must make capacity, allocation, and reuse policy visible.";
    matcher.all = [
      { kind = "field_declaration"; }
      {
        has = {
          kind = "type_identifier";
          regex = "^Vec$";
          stopBy = "end";
        };
      }
    ];
    valid = [
      "pub struct Batch<'items> { pub items: &'items [Item] }"
      "pub struct InlineBatch<const CAPACITY: usize> { items: [Option<Item>; CAPACITY] }"
    ];
    invalid = [
      "pub struct Batch { pub items: Vec<Item> }"
      "struct Stored { entries: std::vec::Vec<Entry> }"
    ];
  };

  no-stringly-error-field = mkRustRule "no-stringly-error-field" {
    category = "diagnostic";
    severity = "error";
    scope = "error-like static string fields named detail, field, phase, step, expected, or observed";
    exception = "bounded untrusted text carried beside a closed typed cause";
    message = "Replace stringly diagnostic facts with a closed enum or typed source.";
    note = "Error fields must remain exhaustively matchable and cannot encode control state in prose.";
    matcher = {
      all = [
        {
          kind = "field_declaration";
          regex = "(?s)(detail|field|phase|step|expected|observed)\\s*:\\s*&'static\\s+str";
        }
      ];
    };
    valid = [
      "pub struct Failure { pub phase: Phase, pub cause: Cause }"
      "pub struct Rejection { pub field: Field, pub observed: ObservedValue }"
    ];
    invalid = [
      "pub struct Failure { pub detail: &'static str }"
      "struct Mismatch { expected: &'static str, observed: &'static str }"
    ];
    snapshotCounts = [
      1
      2
    ];
  };

  no-trivial-delegation = mkRustRule "no-trivial-delegation" {
    category = "abstraction";
    severity = "warning";
    scope = "inherent zero-argument methods that only delegate through one field";
    exception = "foreign trait implementation or visibility-changing safety boundary";
    message = "Remove the pass-through method or redesign the boundary it is hiding.";
    note = "A method must add an invariant, transition, conversion, or safety proof beyond field delegation.";
    matcher.pattern = {
      context = "impl Owner { fn $METHOD(&self) -> $RETURN { self.$FIELD.$INNER() } }";
      selector = "function_item";
    };
    valid = [
      "impl Owner { fn advance(&mut self) -> Result<(), Failure> { self.state.advance()?; self.prove() } }"
      "impl AsRef<[u8]> for Owner { fn as_ref(&self) -> &[u8] { &self.bytes } }"
    ];
    invalid = [
      "impl Owner { fn credits(&self) -> usize { self.inner.credits() } }"
      "impl Owner { fn len(&self) -> usize { self.items.len() } }"
    ];
  };

  no-unit-struct-abstraction = mkRustRule "no-unit-struct-abstraction" {
    category = "abstraction";
    severity = "warning";
    scope = "public unit structs";
    exception = "type-level domain witness with a compile-time identity law";
    message = "Use a function, module, enum, or state-bearing type instead of a struct for its own sake.";
    note = "A zero-sized public type must carry a real type-level proof, not merely namespace a constructor.";
    matcher.pattern = "pub struct $NAME;";
    valid = [
      "pub enum HydrationPlan { Resident, Remote(RemotePlan) }"
      "pub struct DomainWitness(core::marker::PhantomData<fn() -> Domain>);"
    ];
    invalid = [
      "pub struct HydrationPlanner;"
      "pub struct Factory;"
    ];
  };

  no-manual-debug-struct = mkRustRule "no-manual-debug-struct" {
    category = "diagnostic";
    severity = "warning";
    scope = "explicit Formatter::debug_struct construction";
    exception = "reviewed secret redaction or compact foreign summary";
    message = "Derive Debug or use a declarative error representation instead of a manual debug_struct ladder.";
    note = "Manual formatting is reserved for deliberate redaction or measured compact summaries.";
    matcher.pattern = "$FORMATTER.debug_struct($NAME)";
    valid = [
      "#[derive(Debug)] struct Failure { cause: Cause }"
      "#[derive(Debug, thiserror::Error)] enum Failure { #[error(transparent)] Source(SourceError) }"
    ];
    invalid = [
      ''let result = formatter.debug_struct("Failure").field("cause", cause).finish();''
      ''formatter.debug_struct("Rejected").field("bytes_len", &bytes.len()).finish()''
    ];
  };

  no-panic-macro = mkRustRule "no-panic-macro" {
    category = "reliability";
    severity = "error";
    scope = "explicit panic, todo, unreachable, and unimplemented macros";
    exception = "none";
    message = "Represent the terminal as a typed result instead of a panic macro.";
    note = "Production and test harnesses must preserve exact failure information without unwinding.";
    matcher.any = [
      { pattern = "panic!($$$VALUES)"; }
      { pattern = "todo!($$$VALUES)"; }
      { pattern = "unreachable!($$$VALUES)"; }
      { pattern = "unimplemented!($$$VALUES)"; }
    ];
    valid = [
      "return Err(Failure::Unsupported { operation });"
      "let value = option.ok_or(Failure::Missing)?;"
    ];
    invalid = [
      ''panic!("unexpected state")''
      ''unreachable!("closed enum")''
      ''todo!("finish parser")''
    ];
  };

  no-panicking-convenience = mkRustRule "no-panicking-convenience" {
    category = "reliability";
    severity = "error";
    scope = "explicit unwrap and expect method calls";
    exception = "none";
    message = "Propagate or classify the failure instead of unwrapping or expecting.";
    note = "Tests need typed harness errors too, so a failed proof retains its exact cause.";
    matcher.any = [
      { pattern = "$VALUE.unwrap()"; }
      { pattern = "$VALUE.expect($MESSAGE)"; }
    ];
    valid = [
      "let value = operation()?;"
      "let value = option.ok_or(Failure::Missing)?;"
    ];
    invalid = [
      ''let value = operation().expect("operation");''
      "let value = option.unwrap();"
    ];
  };

  no-tuple-index-projection = mkRustRule "no-tuple-index-projection" {
    category = "typing";
    severity = "warning";
    scope = "numeric tuple field projections";
    exception = "macro-generated or reviewed foreign tuple adapter";
    message = "Name the tuple facts with a record or domain type instead of numeric projection.";
    note = "Named facts make ownership, units, and state transitions reviewable at the use site.";
    matcher.any = [
      { pattern = "$VALUE.0"; }
      { pattern = "$VALUE.1"; }
    ];
    valid = [
      "let promised = counts.promised;"
      "let (promised, overlaid) = counts;"
    ];
    invalid = [
      "let promised = counts.0;"
      "let overlaid = counts.1;"
    ];
  };

  no-box-dyn-error = mkRustRule "no-box-dyn-error" {
    category = "diagnostic";
    severity = "error";
    scope = "dynamic Error types owned by Box";
    exception = "foreign ABI quarantine";
    message = "Return a closed typed error instead of Box<dyn Error>.";
    note = "Preserve exact causes in an enum; quarantine unavoidable foreign erasure at its adapter.";
    matcher = {
      all = [
        { kind = "dynamic_type"; }
        {
          has = {
            kind = "type_identifier";
            regex = "^Error$";
            stopBy = "end";
          };
        }
        {
          inside = {
            all = [
              { kind = "generic_type"; }
              {
                has = {
                  kind = "type_identifier";
                  regex = "^Box$";
                };
              }
            ];
            stopBy = "end";
          };
        }
      ];
    };
    valid = [
      "fn run() -> Result<(), Failure> { Ok(()) }"
      "struct Foreign<'error> { source: &'error dyn core::error::Error }"
    ];
    invalid = [
      "fn run() -> Result<(), Box<dyn std::error::Error>> { Ok(()) }"
      "struct Failure { source: Box<dyn core::error::Error> }"
    ];
  };

  no-default-error-fallback = mkRustRule "no-default-error-fallback" {
    category = "diagnostic";
    severity = "error";
    scope = "explicit unwrap_or_default calls";
    exception = "named domain recovery adapter";
    message = "Replace unwrap_or_default with an explicit typed recovery policy.";
    note = "Defaults must be named domain decisions, not silent error or absence erasure.";
    matcher = {
      pattern = "$VALUE.unwrap_or_default()";
    };
    valid = [
      "let value = option.unwrap_or_else(explicit_empty_value);"
      "let value = result.map_err(Failure::Source)?;"
    ];
    invalid = [
      "let value = option.unwrap_or_default();"
      "let value = result.unwrap_or_default();"
    ];
  };

  no-discarded-result = mkRustRule "no-discarded-result" {
    category = "diagnostic";
    severity = "error";
    scope = "explicit wildcard let bindings";
    exception = "typed cleanup terminal";
    message = "Classify or propagate a fallible result instead of discarding it.";
    note = "Use a typed recovery branch, propagation, or an explicit terminal result.";
    matcher = {
      pattern = "let _ = $EXPRESSION;";
    };
    valid = [
      "fn handle() -> Result<(), Failure> { operation()?; Ok(()) }"
      "fn classify() { if let Err(source) = operation() { report(source); } }"
      "fn explicitly_ignore_infallible() { let _unit = (); }"
    ];
    invalid = [
      "fn discard() { let _ = operation(); }"
      "fn discard_join() { let _ = worker.join(); }"
    ];
  };

  no-dynamic-json = mkRustRule "no-dynamic-json" {
    category = "protocol";
    severity = "error";
    scope = "serde_json::json and json macro calls";
    exception = "untrusted-envelope adapter";
    message = "Use a closed serde data transfer type instead of constructing dynamic JSON.";
    note = "Raw JSON values belong only in a quarantined untrusted-envelope decoder.";
    matcher.any = [
      { pattern = "serde_json::json!($$$VALUES)"; }
      { pattern = "json!($$$VALUES)"; }
    ];
    valid = [
      "let body = serde_json::to_vec(&Request { operation })?;"
      "let body = serde_json::to_value(Request { operation })?;"
    ];
    invalid = [
      ''let body = serde_json::json!({ "operation": operation });''
      ''let body = json!({ "operation": operation });''
      "let body = serde_json::json!([operation, fallback]);"
    ];
  };

  no-lib-implementation = mkRustRule "no-lib-implementation" {
    category = "architecture";
    severity = "error";
    scope = "top-level implementation items in lib.rs";
    exception = "none";
    message = "Move implementation out of lib.rs into an invariant-named module.";
    note = "Crate roots own documentation, attributes, module declarations, imports, and reexports only.";
    files = [ "**/lib.rs" ];
    matcher = {
      all = [
        {
          any = map (kind: { inherit kind; }) [
            "const_item"
            "enum_item"
            "function_item"
            "impl_item"
            "macro_definition"
            "static_item"
            "struct_item"
            "trait_item"
            "type_item"
            "union_item"
          ];
        }
        {
          inside = {
            kind = "source_file";
          };
        }
      ];
    };
    valid = [
      "//! Purpose.\n//! Boundary.\n//! Guarantee.\nmod model;\npub use model::Model;"
      "#![no_std]\nmod model;\nuse model::Model;"
    ];
    invalid = [
      "pub struct Model { value: usize }"
      "impl Model { pub fn value(&self) -> usize { self.value } }"
    ];
  };

  no-locking-primitives = mkRustRule "no-locking-primitives" {
    category = "concurrency";
    severity = "error";
    scope = "Mutex, RwLock, and Condvar type identifiers";
    exception = "foreign adapter quarantine";
    message = "Shared mutable locks require a semantic redesign, not routine admission.";
    note = "Prefer ownership transfer, immutable snapshots, atomics, or a proved lock-free structure.";
    matcher = {
      kind = "type_identifier";
      regex = "^(Mutex|RwLock|Condvar)$";
    };
    valid = [
      "struct Shared { state: AtomicUsize }"
      "struct Owner { channel: Sender<Event> }"
      "struct Local { state: Cell<State> }"
    ];
    invalid = [
      "struct Shared { state: Mutex<State> }"
      "struct Shared { state: Arc<RwLock<State>> }"
      "struct Shared { state: parking_lot::Mutex<State> }"
      "struct Gate(std::sync::Condvar);"
    ];
  };

  no-manual-enum-string-projector = mkRustRule "no-manual-enum-string-projector" {
    category = "vocabulary";
    severity = "warning";
    scope = "free enum-to-static-string match functions";
    exception = "behavior-bearing formatter";
    message = "Derive the enum string representation instead of maintaining a match projector.";
    note = "Prefer a reviewed strum or serde representation owned by the enum.";
    matcher.pattern = {
      context = "fn $METHOD(value: $TYPE) -> &'static str {\n  match value {\n    $$$ARMS\n  }\n}";
      selector = "function_item";
    };
    valid = [
      "#[derive(strum::Display)] enum InputClass { Local, Remote }"
      "impl core::fmt::Display for InputClass { fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { formatter.write_str(self.as_ref()) } }"
    ];
    invalid = [
      ''fn input_class_name(value: InputClass) -> &'static str { match value { InputClass::Local => "local", InputClass::Remote => "remote" } }''
      ''fn resource_name(value: Resource) -> &'static str { match value { Resource::Ram => "ram", Resource::Nvme => "nvme", Resource::Cpu => "cpu" } }''
    ];
  };

  no-public-primitive-field = mkRustRule "no-public-primitive-field" {
    category = "typing";
    severity = "warning";
    scope = "public named and tuple fields containing primitive syntax";
    exception = "reviewed wire or FFI scalar";
    message = "Give the public scalar a domain type or a reviewed wire-boundary waiver.";
    note = "Names, ranges, units, and invalid states should be encoded by the type.";
    matcher.any = [
      {
        all = [
          { kind = "primitive_type"; }
          {
            inside = {
              all = [
                { kind = "field_declaration"; }
                {
                  has = {
                    kind = "visibility_modifier";
                  };
                }
              ];
              stopBy = "end";
            };
          }
        ];
      }
      {
        all = [
          { kind = "primitive_type"; }
          {
            inside = {
              all = [
                { kind = "ordered_field_declaration_list"; }
                {
                  has = {
                    kind = "visibility_modifier";
                  };
                }
              ];
              stopBy = "end";
            };
          }
        ];
      }
      {
        pattern = {
          context = "struct Example { pub $FIELD: String }";
          selector = "field_declaration";
        };
      }
    ];
    valid = [
      "pub struct Request { pub operation: OperationKey }"
      "struct WireHeader { byte_len: u64 }"
      "pub struct Request(pub OperationKey);"
    ];
    invalid = [
      "pub struct Request { pub operation: u64 }"
      "pub struct State { pub ready: bool }"
      "pub struct Retry(pub u8);"
      "pub struct State { pub attempts: Option<u16> }"
    ];
    corpus = {
      count = 3;
      source = ''
        //! Supplies named, nested, and tuple public scalar fields for lint meta-tests.
        //! Keeps private scalar storage beside them as a near-neighbor.
        //! Proves diagnostics count semantic boundary shapes rather than files.

        pub struct PublicFields {
            pub count: u64,
            pub attempts: Option<u16>,
            private: u8,
        }

        pub struct TupleField(pub bool);
      '';
    };
  };

  no-result-ok = mkRustRule "no-result-ok" {
    category = "diagnostic";
    severity = "error";
    scope = "explicit .ok() calls";
    exception = "none";
    message = "Classify or propagate the error instead of converting it to absence.";
    note = "Use a typed match, propagation, or a terminal that retains the cause.";
    matcher.pattern = "$RESULT.ok()";
    valid = [
      "let value = operation()?;"
      "let value = match operation() { Ok(value) => Some(value), Err(source) => return Err(Failure::Source { source }) };"
    ];
    invalid = [
      "let value = operation().ok();"
      "let value = source.parse::<usize>().ok();"
    ];
  };

  no-single-letter-generics = mkRustRule "no-single-letter-generics" {
    category = "typing";
    severity = "error";
    scope = "uppercase single-letter type parameter declarations";
    exception = "none";
    message = "Generic parameters must name the semantic role they abstract.";
    note = "Use names such as DomainTag, Owner, Generation, Work, or Storage.";
    matcher = {
      all = [
        { kind = "type_parameter"; }
        {
          has = {
            kind = "type_identifier";
            regex = "^[A-Z]$";
          };
        }
      ];
    };
    valid = [
      "pub struct Store<Owner, DomainTag> { owner: Owner, marker: PhantomData<DomainTag> }"
      "fn consume(value: TitledValue) {}"
    ];
    invalid = [
      "pub struct Store<K, W> { key: K, work: W }"
      "fn map<T, U>(value: T) -> U { todo!() }"
    ];
    snapshotCounts = [
      2
      2
    ];
    corpus = {
      count = 3;
      source = ''
        //! Supplies a count-sensitive generic-parameter mutant for lint meta-tests.
        //! Keeps three independent matches inside one syntactically valid declaration.
        //! Proves the rule cannot silently stop after its first diagnostic.

        pub struct Store<A, K, W> {
            first: A,
            key: K,
            work: W,
        }
      '';
    };
  };

  no-string-error-detail = mkRustRule "no-string-error-detail" {
    category = "diagnostic";
    severity = "error";
    scope = "common stringly enum error fields";
    exception = "opaque foreign payload with typed provenance";
    message = "Replace the stringly diagnostic field with a closed typed cause.";
    note = "Model phase, expected value, observed value, or foreign provenance as an enum or structured fact.";
    matcher = {
      all = [
        { kind = "field_identifier"; }
        { regex = "^(detail|expected|message|observed|step)$"; }
        {
          inside = {
            all = [
              { kind = "field_declaration"; }
              {
                has = {
                  any = [
                    {
                      kind = "type_identifier";
                      regex = "^String$";
                    }
                    {
                      kind = "primitive_type";
                      regex = "^str$";
                    }
                  ];
                  stopBy = "end";
                };
              }
              {
                inside = {
                  kind = "enum_variant";
                  stopBy = "end";
                };
              }
            ];
          };
        }
      ];
    };
    valid = [
      "enum Failure { Broken { cause: ParseCause } }"
      "struct Message { message: String }"
    ];
    invalid = [
      "enum Failure { Broken { detail: String } }"
      "enum Failure { Mismatch { expected: &'static str, observed: &'static str } }"
    ];
    snapshotCounts = [
      1
      2
    ];
  };

  no-trivial-getter = mkRustRule "no-trivial-getter" {
    category = "api";
    severity = "warning";
    scope = "public inherent direct-field accessors";
    exception = "trait or validated view";
    message = "Expose the field or a transparent view instead of a logic-free getter.";
    note = "Public fields are preferred when construction already guarantees their invariant.";
    matcher.any =
      map
        (context: {
          pattern = {
            inherit context;
            selector = "function_item";
          };
        })
        [
          "impl Example { pub fn $METHOD(&self) -> $RETURN { self.$FIELD } }"
          "impl Example { pub fn $METHOD(&self) -> $RETURN { &self.$FIELD } }"
          "impl Example { pub const fn $METHOD(self) -> $RETURN { self.$FIELD } }"
          "impl Example { pub const fn $METHOD(&self) -> $RETURN { &self.$FIELD } }"
        ];
    valid = [
      "impl Bytes { pub fn validated(&self) -> Result<&[u8], Error> { validate(&self.bytes)?; Ok(&self.bytes) } }"
      "impl AsRef<[u8]> for Bytes { fn as_ref(&self) -> &[u8] { &self.bytes } }"
      "impl Bytes { pub fn length(&self) -> usize { self.bytes.len() } }"
    ];
    invalid = [
      "impl Bytes { pub fn bytes(&self) -> &[u8] { self.bytes } }"
      "impl ByteCount { pub const fn get(self) -> u64 { self.0 } }"
      "impl Bytes { pub const fn content(&self) -> &[u8] { &self.content } }"
    ];
  };

  no-trivial-setter = mkRustRule "no-trivial-setter" {
    category = "api";
    severity = "warning";
    scope = "public direct-field mutation methods";
    exception = "validated transition";
    message = "Model the state transition or expose the field instead of a direct setter.";
    note = "A mutation method must validate, normalize, or encode a meaningful transition.";
    matcher.pattern = {
      context = "impl Example {\n  pub fn $METHOD(&mut self, value: $TYPE) {\n    self.$FIELD = value;\n  }\n}";
      selector = "function_item";
    };
    valid = [
      "impl Budget { pub fn reserve(&mut self, value: Count) -> Result<(), Error> { self.remaining = self.remaining.checked_sub(value)?; Ok(()) } }"
      "impl Budget { pub fn replace(&mut self, value: Count) -> Count { core::mem::replace(&mut self.remaining, value) } }"
    ];
    invalid = [
      "impl Budget { pub fn set_remaining(&mut self, value: Count) { self.remaining = value; } }"
      "impl Budget { pub fn set_limit(&mut self, value: Count) { self.limit = value; } }"
    ];
  };

  preserve-error-cause = mkRustRule "preserve-error-cause" {
    category = "diagnostic";
    severity = "error";
    scope = "map_err closures that syntactically discard their source";
    exception = "none";
    message = "Preserve the rejected error in a typed cause instead of discarding it.";
    note = "Replace the wildcard with a named source and carry it in the returned error.";
    matcher.any = [
      { pattern = "$VALUE.map_err(|_| $ERROR)"; }
      { pattern = "$VALUE.map_err(|$IGNORED| $ERROR)"; }
      { pattern = "$VALUE.map_err(drop)"; }
    ];
    constraints.IGNORED.regex = "^_[A-Za-z0-9_]*$";
    valid = [
      ''
        fn retain() -> Result<(), Failure> {
          source().map_err(|source| Failure::Source { source })
        }
      ''
      ''
        fn propagate() -> Result<(), Failure> {
          source()?;
          Ok(())
        }
      ''
      "fn convert() -> Result<(), Failure> { source().map_err(Failure::from) }"
    ];
    invalid = [
      ''
        fn erase() -> Result<(), Failure> {
          source().map_err(|_| Failure::Allocation)
        }
      ''
      "fn erase_named() -> Result<(), Failure> { source().map_err(|_source| Failure::Allocation) }"
      "fn erase_with_drop() -> Result<(), ()> { source().map_err(drop) }"
    ];
    corpus = {
      count = 3;
      source = ''
        //! Supplies three distinct cause-erasure forms for lint meta-tests.
        //! Exercises wildcard, underscore-named, and function-pointer disposal.
        //! Proves all configured syntax branches emit independent diagnostics.

        fn erase() {
            source().map_err(|_| Failure::Allocation);
            source().map_err(|_source| Failure::Allocation);
            source().map_err(drop);
        }
      '';
    };
  };

  no-manual-serde-visitor = mkRustRule "no-manual-serde-visitor" {
    category = "protocol";
    severity = "warning";
    scope = "manual serde Visitor implementations";
    exception = "proved streaming grammar whose typed cause precedence cannot be expressed by derive, serde remote, or a parser combinator";
    message = "Use a closed derived wire type or parser combinator instead of a hand-maintained serde visitor.";
    note = "Manual visitors routinely grow flag bags and duplicate state machines; the exception must name the grammar and allocation bound it preserves.";
    matcher.any = [
      { pattern = "impl<'de> serde::de::Visitor<'de> for $TYPE { $$$ITEMS }"; }
      { pattern = "impl<'de> Visitor<'de> for $TYPE { $$$ITEMS }"; }
    ];
    valid = [
      ''#[derive(serde::Deserialize)] #[serde(tag = "action", deny_unknown_fields)] enum Request { Build { source: String } }''
      "fn decode(input: &[u8]) -> Result<Request, DecodeError> { parser::request(input) }"
    ];
    invalid = [
      "impl<'de> serde::de::Visitor<'de> for RequestVisitor { type Value = Request; fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { formatter.write_str(\"request\") } }"
      "impl<'de> Visitor<'de> for Fields { type Value = Fields; fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error> where M: MapAccess<'de> { decode(map) } }"
    ];
  };

  no-boxed-slice-field = mkRustRule "no-boxed-slice-field" {
    category = "allocation";
    severity = "warning";
    scope = "boxed slice fields";
    exception = "measured thin immutable owner that outperforms an inline, borrowed, arena, slab, mmap, or caller-owned representation";
    message = "Name and prove the allocation topology instead of defaulting a collection field to Box<[T]>.";
    note = "A boxed slice is earned only when its one-word owner and exact capacity are the measured representation choice.";
    matcher = {
      kind = "field_declaration";
      regex = "Box\\s*<\\s*\\[";
    };
    valid = [
      "pub struct Rows<'rows> { pub rows: &'rows [Row] }"
      "pub struct InlineRows<const CAPACITY: usize> { rows: [Row; CAPACITY] }"
    ];
    invalid = [
      "pub struct Routes { routes: Box<[Route]> }"
      "struct Objects { objects: std::boxed::Box<[Object]> }"
    ];
  };

  no-lint-allow = mkRustRule "no-lint-allow" {
    category = "proof";
    severity = "error";
    scope = "Rust lint-level attributes";
    exception = "generated or vendored source outside the owned tree";
    message = "Use a reasoned expect attribute that proves a live finding instead of permanently allowing a lint.";
    note = "Expectations fail when the finding disappears; allow attributes silently survive refactors and accumulate policy debt.";
    matcher = {
      kind = "attribute_item";
      regex = "^#\\[allow\\(";
    };
    valid = [
      ''#[expect(clippy::result_large_err, reason = "cold typed rejection retains the returned owner")] fn run() {}''
      "#[deny(unsafe_op_in_unsafe_fn)] fn checked() {}"
    ];
    invalid = [
      "#[allow(dead_code)] fn unused() {}"
      "#[allow(clippy::large_enum_variant)] enum Reply { Small, Large([u8; 256]) }"
    ];
  };

  no-unreasoned-expect = mkRustRule "no-unreasoned-expect" {
    category = "proof";
    severity = "error";
    scope = "Rust lint expectations";
    exception = "none";
    message = "State the invariant or measured tradeoff that makes this lint expectation necessary.";
    note = "A reason is reviewable evidence; an unnamed suppression is indistinguishable from accidental lint avoidance.";
    matcher.all = [
      {
        kind = "attribute_item";
        regex = "^#\\[expect\\(";
      }
      {
        not.regex = "reason\\s*=";
      }
    ];
    valid = [
      ''#[expect(clippy::result_large_err, reason = "cold error retains exact authority snapshots")] fn run() {}''
      ''#[expect(unsafe_share_contract, reason = "bounded queue has Loom and Miri proof")] unsafe impl Sync for Queue {}''
    ];
    invalid = [
      "#[expect(dead_code)] fn unused() {}"
      "#[expect(clippy::large_enum_variant)] enum Reply { Small, Large([u8; 256]) }"
    ];
  };
}
