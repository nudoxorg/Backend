use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TextIndexError {
	#[error("create directory `{path}`")]
	CreateDir {
		path:   PathBuf,
		#[source]
		source: io::Error,
	},

	#[error("mmap directory at `{path}`")]
	MmapDirectory {
		path:   PathBuf,
		#[source]
		source: tantivy::directory::error::OpenDirectoryError,
	},

	#[error("open or create tantivy index")]
	OpenOrCreate {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("create index writer")]
	Writer {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("writer lock poisoned")]
	LockPoisoned,

	#[error("add document")]
	AddDocument {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("commit")]
	Commit {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("create reader")]
	Reader {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("parse query `{query}`")]
	ParseQuery {
		query:  String,
		#[source]
		source: tantivy::query::QueryParserError,
	},

	#[error("execute search")]
	Search {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("fetch document")]
	DocFetch {
		#[source]
		source: tantivy::TantivyError,
	},
}
