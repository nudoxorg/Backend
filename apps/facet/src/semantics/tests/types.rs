//! The type parser and the plain-word speller, over real fixture types.

use crate::semantics::types::{
    Piece, Resolve, Scope, Target, TypeExpr, parse, parse_fields, parse_list, split_top,
};
use gpui::SharedString;

/// Resolves bare names from a table; a qualified path only when the table
/// names it whole. `Result` with one argument is the serde_json alias.
struct Known(&'static [(&'static str, u32)]);

impl Resolve for Known {
    fn named(&self, path: &[String]) -> Option<Target> {
        let joined = path.join("::");
        self.0.iter().find(|(name, _)| *name == joined).map(|(_, id)| Target::Node(*id))
    }

    fn alias(&self, path: &[String], arity: usize) -> Option<(Vec<String>, TypeExpr)> {
        (path.len() == 1 && path[0] == "Result" && arity == 1)
            .then(|| (vec!["T".to_owned()], parse("result::Result<T, Error>")))
    }
}

const KNOWN: &[(&str, u32)] = &[
    ("RelationLabel", 1),
    ("Relation", 2),
    ("Advisory", 3),
    ("Deserialize", 4),
    ("Error", 5),
    ("AuthorityParseError", 6),
    ("CanonicalAdvisoryId", 7),
    ("FactRecord", 8),
    ("SemanticLinkKind", 9),
    ("de::Deserialize", 4),
];

fn scope() -> Scope<'static> {
    Scope::new(&Known(KNOWN))
        .generics(["K", "V", "R", "Key"])
        .owner(Target::Node(3), "Advisory")
}

/// `(source, plain words)`: every entry is a type that occurs in the fixture.
const TABLE: &[(&str, &str)] = &[
    // lists, slices, arrays
    ("&[u8]", "list of u8"),
    ("[u8; 0]", "0 × u8"),
    ("Box<[String]>", "list of text"),
    ("Box<[&'a Advisory]>", "list of Advisory"),
    ("&[Advisory]", "list of Advisory"),
    ("Vec<Vec<u8>>", "list of list of u8"),
    ("[[f32; 4]; 4]", "4 × 4 × f32"),
    ("VecDeque<ExecutableCacheEntry>", "list of ExecutableCacheEntry"),
    // maybe
    ("Option<String>", "maybe text"),
    ("Option<&Advisory>", "maybe Advisory"),
    ("Option<&str>", "maybe text"),
    ("Option<u16>", "maybe u16"),
    ("Option<&toml::Value>", "maybe Value"),
    ("Option<backend_replication::AuthenticatedLocalPeer>", "maybe AuthenticatedLocalPeer"),
    ("Option<Self::Item>", "maybe its Item"),
    // results
    ("Result<Self, AuthorityParseError>", "Advisory or fails with AuthorityParseError"),
    ("Result<Vec<Advisory>, AuthorityParseError>", "list of Advisory or fails with AuthorityParseError"),
    ("Result<Self, &'static str>", "Advisory or fails with text"),
    ("Result<&'a str, ParseError>", "text or fails with ParseError"),
    ("Result<(), AuthorityStorageError>", "nothing or fails with AuthorityStorageError"),
    ("Result<Vec<VersionEvent>, ()>", "list of VersionEvent or fails with nothing"),
    ("Result<(String, VersionKey), VersionCompareError>", "text × VersionKey or fails with VersionCompareError"),
    ("Result<ProducerObservationClaims, Self::Error>", "ProducerObservationClaims or fails with its Error"),
    ("Result<Self::Parsed<'record>, Self::Error>", "its Parsed or fails with its Error"),
    ("Result<Self::Key, backend_version::RelationDecodeError>", "its Key or fails with RelationDecodeError"),
    ("Result<Self::Value, E>", "its Value or fails with E"),
    ("Result<Self::Value, D::Error>", "its Value or fails with D’s Error"),
    ("Result<Self::Value, ()>", "its Value or fails with nothing"),
    // aliases of Result
    ("Result<T>", "T or fails with Error"),
    ("fmt::Result", "nothing or fails with Error"),
    ("std::io::Result<Vec<u8>>", "list of u8 or fails with Error"),
    // borrows, pointers
    ("&mut std::fmt::Formatter<'_>", "mutable Formatter"),
    ("&mut fmt::Formatter<'_>", "mutable Formatter"),
    ("&'a mut E", "mutable E"),
    ("&mut Vec<u8>", "mutable list of u8"),
    ("*const u8", "pointer to u8"),
    ("&Self::Key", "its Key"),
    // text and paths
    ("std::path::PathBuf", "path"),
    ("Cow<'a, str>", "text"),
    ("Cow<'jdk, Path>", "path"),
    ("HashSet<&'static str>", "set of text"),
    ("Arc<str>", "shared text"),
    // maps
    ("BTreeMap<String, String>", "map text → text"),
    ("HashMap<PreparationKey, usize>", "map PreparationKey → usize"),
    ("&BTreeMap<Arc<str>, FactObservation>", "map shared text → FactObservation"),
    ("BTreeMap<CanonicalAdvisoryId, Box<[WithdrawalRecord]>>", "map CanonicalAdvisoryId → list of WithdrawalRecord"),
    ("BTreeMap<String, BTreeMap<String, BTreeSet<PackageIdentity>>>", "map text → map text → set of PackageIdentity"),
    // shared, locked, changeable
    ("Arc<Mutex<Vec<u8>>>", "shared locked list of u8"),
    ("Mutex<Option<Result<(), PreparationError>>>", "locked maybe nothing or fails with PreparationError"),
    ("&Mutex<T>", "locked T"),
    ("Rc<RefCell<Vec<Node>>>", "shared changeable list of Node"),
    ("Arc<AtomicBool>", "shared AtomicBool"),
    ("MutexGuard<'_, T>", "MutexGuard‹T›"),
    // nested generics
    ("Arc<[RelationState<FactRelation<K, V>>]>", "shared list of RelationState‹FactRelation‹K, V››"),
    ("OnceLock<Arc<[FactRecord<K, V>]>>", "OnceLock‹shared list of FactRecord‹K, V››"),
    (
        "Result<(CompleteAuthorityCoverage, Vec<FactRecord<K, V>>), AuthorityAdmissionError>",
        "CompleteAuthorityCoverage × list of FactRecord‹K, V› or fails with AuthorityAdmissionError",
    ),
    // tuples
    ("(AdvisoryObservation, AcquisitionDecision)", "AdvisoryObservation × AcquisitionDecision"),
    ("(&str, &str)", "text × text"),
    ("(Self, CancelHandle)", "Advisory × CancelHandle"),
    (
        "(CoverageWitness, Arc<FactSet<K, V>>, Option<Arc<AuthorityFence>>,)",
        "CoverageWitness × shared FactSet‹K, V› × maybe shared AuthorityFence",
    ),
    ("()", "nothing"),
    // dyn and impl
    ("impl IntoIterator<Item = AdvisorySource>", "any IntoIterator‹AdvisorySource›"),
    ("impl AsRef<Path>", "any AsRef‹path›"),
    ("impl Into<String>", "any Into‹text›"),
    ("impl Into<Box<[Relation]>>", "any Into‹list of Relation›"),
    ("&mut impl Write", "mutable any Write"),
    ("impl Iterator<Item = usize> + 'a", "any Iterator‹usize›"),
    ("impl fmt::Display", "any Display"),
    ("Result<SessionKey, Box<dyn Error>>", "SessionKey or fails with any Error"),
    ("Option<&(dyn std::error::Error + 'static)>", "maybe any Error"),
    ("Option<Box<dyn RemoteTransport>>", "maybe any RemoteTransport"),
    ("Arc<dyn CasNodeReader<R, Error = StoreError> + Send + Sync>", "shared any CasNodeReader‹R, StoreError›"),
    ("Pin<Box<dyn Future<Output = T> + Send + 'a>>", "any Future‹T›"),
    ("impl Read + Send", "any Read"),
    // functions
    ("&'a dyn Fn() -> bool", "a function of nothing → bool"),
    ("PhantomData<fn() -> (K, V)>", "a marker for a function of nothing → K × V"),
    ("std::marker::PhantomData<fn() -> T>", "a marker for a function of nothing → T"),
    ("Box<dyn Fn(&str) -> Result<(), Error> + Send>", "a function of text → nothing or fails with Error"),
    ("impl FnMut(A) -> B", "a function of A → B"),
    ("fn(&mut Formatter) -> fmt::Result", "a function of mutable Formatter → nothing or fails with Error"),
    ("for<'a> fn(&'a str) -> &'a str", "a function of text → text"),
    // projections, never, inference, plain names
    ("<T as Iterator>::Item", "T’s Item"),
    ("!", "never returns"),
    ("_", "_"),
    ("RelationLabel", "RelationLabel"),
    ("SemanticLinkKind", "SemanticLinkKind"),
    ("Self", "Advisory"),
    ("self", "it"),
    ("Key", "Key"),
];

#[test]
fn every_fixture_type_reads_in_plain_words() {
    let scope = scope();
    let mut wrong = Vec::new();
    for (source, want) in TABLE {
        let got = scope.spell_text(source).plain();
        if got != *want {
            wrong.push(format!("{source:?}\n    want {want:?}\n    got  {got:?}"));
        }
    }
    assert!(TABLE.len() >= 60, "the table must cover at least 60 real types");
    assert!(wrong.is_empty(), "{} of {} spelled wrong:\n{}", wrong.len(), TABLE.len(), wrong.join("\n"));
}

fn names(source: &str) -> Vec<(String, Target)> {
    scope()
        .spell_text(source)
        .pieces
        .into_iter()
        .filter_map(|p| match p {
            Piece::Name { text, target } => Some((text.to_string(), target)),
            _ => None,
        })
        .collect()
}

fn path(p: &str) -> Target {
    Target::Path(SharedString::from(p.to_owned()))
}

#[test]
fn every_named_type_is_a_link_and_generics_never_are() {
    assert_eq!(names("Result<Vec<Advisory>, AuthorityParseError>"), [("Advisory".into(), Target::Node(3)), ("AuthorityParseError".into(), Target::Node(6))]);
    // Unknown to the world: still a link, by its full path.
    assert_eq!(names("&mut std::fmt::Formatter<'_>"), [("Formatter".into(), path("std::fmt::Formatter"))]);
    // A qualified path the world names whole resolves.
    assert_eq!(names("de::Deserialize<'a>"), [("Deserialize".into(), Target::Node(4))]);
    // Generics, associated types, primitives and plain words are not links.
    assert!(names("Result<Self::Value, E>").is_empty());
    assert!(names("FactRecord<K, V>").iter().all(|(n, _)| n == "FactRecord"));
    assert!(names("Option<u64>").is_empty());
    // `Self` is its owner.
    assert_eq!(names("Self"), [("Advisory".into(), Target::Node(3))]);
    // The serde_json alias's error resolves where the alias lives.
    assert_eq!(names("Result<T>"), [("Error".into(), Target::Node(5))]);
    assert_eq!(names("fmt::Result"), [("Error".into(), path("fmt::Error"))]);
}

#[test]
fn a_type_keeps_its_exact_source_for_xray() {
    let spelled = scope().spell_text("  &mut fmt::Formatter<'_> ");
    assert_eq!(spelled.source.as_ref(), "&mut fmt::Formatter<'_>");
    assert_eq!(spelled.plain(), "mutable Formatter");
}

fn named(path: &[&str], args: Vec<TypeExpr>) -> TypeExpr {
    TypeExpr::Named { path: path.iter().map(|s| (*s).to_owned()).collect(), args }
}

#[test]
fn the_parser_builds_the_language_neutral_tree() {
    assert_eq!(
        parse("Result<Self::Parsed<'record>, backend_version::RelationDecodeError>"),
        named(
            &["Result"],
            vec![
                TypeExpr::Assoc { base: Box::new(named(&["Self"], vec![])), via: None, name: "Parsed".into() },
                named(&["backend_version", "RelationDecodeError"], vec![]),
            ]
        )
    );
    assert_eq!(
        parse("&'a mut [u8; 4]"),
        TypeExpr::Ref {
            mutable: true,
            inner: Box::new(TypeExpr::Array { inner: Box::new(named(&["u8"], vec![])), len: "4".into() })
        }
    );
    assert_eq!(
        parse("impl Iterator<Item = &'a str> + Send + 'a"),
        TypeExpr::Any(vec![
            named(
                &["Iterator"],
                vec![TypeExpr::Binding {
                    name: "Item".into(),
                    ty: Box::new(TypeExpr::Ref { mutable: false, inner: Box::new(named(&["str"], vec![])) })
                }]
            ),
            named(&["Send"], vec![]),
        ])
    );
    assert_eq!(
        parse("Box<dyn FnOnce(u8, &str) -> bool>"),
        named(
            &["Box"],
            vec![TypeExpr::Any(vec![TypeExpr::Func {
                params: vec![named(&["u8"], vec![]), TypeExpr::Ref { mutable: false, inner: Box::new(named(&["str"], vec![])) }],
                ret: Some(Box::new(named(&["bool"], vec![]))),
            }])]
        )
    );
    assert_eq!(
        parse("<T as de::Deserialize<'de>>::Value"),
        TypeExpr::Assoc {
            base: Box::new(named(&["T"], vec![])),
            via: Some(Box::new(named(&["de", "Deserialize"], vec![]))),
            name: "Value".into()
        }
    );
    // `(T)` is T; `(T,)` is a one-tuple; `()` is nothing.
    assert_eq!(parse("(u8)"), named(&["u8"], vec![]));
    assert_eq!(parse("(u8,)"), TypeExpr::Tuple(vec![named(&["u8"], vec![])]));
    assert_eq!(parse("()"), TypeExpr::Tuple(vec![]));
    assert_eq!(parse("*mut c_void"), TypeExpr::Ptr { mutable: true, inner: Box::new(named(&["c_void"], vec![])) });
    assert_eq!(parse(""), TypeExpr::Tuple(vec![]));
}

#[test]
fn printing_the_tree_gives_rust_back() {
    for (source, printed) in [
        ("Result<Vec<Advisory>, AuthorityParseError>", "Result<Vec<Advisory>, AuthorityParseError>"),
        ("&'a mut [u8; 4]", "&mut [u8; 4]"),
        ("Box<dyn Fn(&str) -> bool + Send>", "Box<impl fn(&str) -> bool + Send>"),
        ("(u8,)", "(u8,)"),
        ("Option<Self::Item>", "Option<Self::Item>"),
        ("<T as Iterator>::Item", "<T as Iterator>::Item"),
        ("*const u8", "*const u8"),
    ] {
        assert_eq!(parse(source).to_string(), printed, "{source}");
        // And the printed text parses to the same tree.
        assert_eq!(parse(printed), parse(source), "{source}");
    }
}

#[test]
fn payload_lists_split_at_the_top_level_only() {
    assert_eq!(parse_list("SemanticLinkKind, RelationDirection").len(), 2);
    assert_eq!(parse_list("BTreeMap<String, Vec<(u8, u8)>>, bool").len(), 2);
    assert_eq!(split_top("a: Vec<(u8, u8)>, b: fn(u8) -> u8, c: [u8; 2]", ','), ["a: Vec<(u8, u8)>", "b: fn(u8) -> u8", "c: [u8; 2]"]);
    let fields = parse_fields("path: std::path::PathBuf, source: std::io::Error");
    assert_eq!(fields[0].0.as_deref(), Some("path"));
    assert_eq!(fields[1].1, named(&["std", "io", "Error"], vec![]));
}
