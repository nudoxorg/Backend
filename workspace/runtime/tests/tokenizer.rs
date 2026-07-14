//! Behavioural coverage for the symbol-search identifier tokenizer: it splits
//! camelCase/snake_case/acronym/path identifiers into their component words so a
//! query for a *part* of a name finds it — which the raw-string index cannot do.
//!
//! Exercises the public API (`IdentifierTokenizer`, `register`, `IDENT_TOKENIZER`)
//! against a real in-memory tantivy index, the same way the live symbol index is
//! configured in `runtime::text::index`.

use tantivy::{
	Index, TantivyDocument, doc,
	collector::TopDocs,
	query::QueryParser,
	schema::{IndexRecordOption, STORED, Schema, TextFieldIndexing, TextOptions, Value},
	tokenizer::{LowerCaser, RemoveLongFilter, TextAnalyzer, TokenStream},
};

use runtime::text::tokenizer::{IDENT_TOKENIZER, IdentifierTokenizer, register};

fn analyze(text: &str) -> Vec<String> {
	let mut analyzer: TextAnalyzer = TextAnalyzer::builder(IdentifierTokenizer)
		.filter(RemoveLongFilter::limit(64))
		.filter(LowerCaser)
		.build();
	let mut stream = analyzer.token_stream(text);
	let mut out = Vec::new();
	while stream.advance() {
		out.push(stream.token().text.clone());
	}
	out
}

#[test]
fn analyzer_splits_and_lowercases_identifiers() {
	assert_eq!(analyze("getUserById"), ["get", "user", "by", "id"]);
	assert_eq!(analyze("HTTPServer"), ["http", "server"]);
	assert_eq!(analyze("read_to_string"), ["read", "to", "string"]);
	assert_eq!(analyze("foo::bar::Baz"), ["foo", "bar", "baz"]);
	assert_eq!(analyze("utf8"), ["utf", "8"]);
	assert_eq!(analyze("simple"), ["simple"]);
}

#[test]
fn subtoken_query_finds_names_by_their_parts() {
	let mut builder = Schema::builder();
	let name_tokens = builder.add_text_field(
		"name_tokens",
		TextOptions::default().set_indexing_options(
			TextFieldIndexing::default()
				.set_tokenizer(IDENT_TOKENIZER)
				.set_index_option(IndexRecordOption::WithFreqsAndPositions),
		),
	);
	let stored = builder.add_text_field("stored", STORED);
	let schema = builder.build();

	let index = Index::create_in_ram(schema);
	register(&index);
	let mut writer = index.writer(15_000_000).unwrap();
	for name in ["getUserById", "HTTPServer", "read_to_string", "unrelated_symbol"] {
		writer.add_document(doc!(name_tokens => name, stored => name)).unwrap();
	}
	writer.commit().unwrap();

	let searcher = index.reader().unwrap().searcher();
	let parser = QueryParser::for_index(&index, vec![name_tokens]);
	let hits = |query: &str| -> Vec<String> {
		let parsed = parser.parse_query(query).unwrap();
		searcher
			.search(&parsed, &TopDocs::with_limit(10))
			.unwrap()
			.into_iter()
			.map(|(_, address)| {
				let document: TantivyDocument = searcher.doc(address).unwrap();
				document.get_first(stored).unwrap().as_str().unwrap().to_string()
			})
			.collect()
	};

	assert!(hits("user").contains(&"getUserById".to_string()));
	assert!(hits("http").contains(&"HTTPServer".to_string()));
	assert!(hits("string").contains(&"read_to_string".to_string()));
	assert!(hits("\"get user\"").contains(&"getUserById".to_string()));
	assert!(!hits("user").contains(&"unrelated_symbol".to_string()));
}

/// C# / .NET names tokenize without the metadata arity backtick pollution.
///
/// CLR generic names carry a `` `N `` suffix (`List`1`, `Dictionary`2`) that
/// must be stripped before word-boundary splitting so arity digits never appear
/// as standalone search tokens.  The other C# patterns (PascalCase interface
/// prefix, operator names, namespaced types) fall naturally out of the existing
/// camelCase / snake_case / dot-separator logic.
#[test]
fn csharp_identifiers_tokenize_without_arity_digits() {
	// Generic arity suffix is stripped; only the base name's sub-words index.
	assert_eq!(analyze("List`1"), ["list"], "arity digit must not become a token");
	assert_eq!(analyze("Dictionary`2"), ["dictionary"], "arity digit must not become a token");
	assert_eq!(
		analyze("IEnumerable`1"),
		["i", "enumerable"],
		"interface-prefix I stays, arity stripped"
	);

	// C# interface convention: leading I + PascalCase body.
	// The `I` splits from the body word at the camelCase hump.
	assert_eq!(analyze("IEnumerable"), ["i", "enumerable"]);
	assert_eq!(analyze("IDisposable"), ["i", "disposable"]);

	// Operator method names use snake_case (op_Addition, op_Implicit, …).
	assert_eq!(analyze("op_Addition"), ["op", "addition"]);
	assert_eq!(analyze("op_Implicit"), ["op", "implicit"]);

	// Fully-qualified C# names use dot separators, mirroring Java.
	assert_eq!(
		analyze("System.Collections.Generic"),
		["system", "collections", "generic"]
	);
	assert_eq!(
		analyze("Microsoft.Extensions.DependencyInjection"),
		["microsoft", "extensions", "dependency", "injection"]
	);
}
