use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
	#[error("clone from `{url}` failed: {source}")]
	Clone {
		url:    String,
		#[source]
		source: gix::clone::Error,
	},

	#[error("failed to open repository at `{path}`: {source}")]
	Open {
		path:   PathBuf,
		#[source]
		source: gix::open::Error,
	},

	#[error("IO error at `{path}`: {source}")]
	Io {
		path:   PathBuf,
		#[source]
		source: io::Error,
	},

	#[error("failed to find git remote: {source}")]
	FindRemote {
		#[source]
		source: gix::remote::find::for_fetch::Error,
	},

	#[error("failed to connect to remote: {source}")]
	Connect {
		#[source]
		source: gix::remote::connect::Error,
	},

	#[error("failed to prepare fetch: {source}")]
	PrepareFetch {
		#[source]
		source: gix::remote::fetch::prepare::Error,
	},

	#[error("failed to receive fetch: {source}")]
	ReceiveFetch {
		#[source]
		source: gix::remote::fetch::Error,
	},

	#[error("fetch/checkout failed: {source}")]
	FetchCheckout {
		#[source]
		source: gix::clone::fetch::Error,
	},

	#[error("worktree checkout failed: {source}")]
	WorktreeCheckout {
		#[source]
		source: gix::clone::checkout::main_worktree::Error,
	},

	#[error("failed to resolve git reference `{name}`: {source}")]
	Reference {
		name:   String,
		#[source]
		source: gix::object::find::existing::with_conversion::Error,
	},

	#[error("failed to decode commit tree id for `{name}`: {source}")]
	CommitDecode {
		name:   String,
		#[source]
		source: gix_object::decode::Error,
	},

	#[error("failed to build index from tree `{tree}`: {source}")]
	IndexFromTree {
		tree:   String,
		#[source]
		source: gix::repository::index_from_tree::Error,
	},

	#[error("failed to obtain checkout options: {0}")]
	CheckoutOptions(#[source] gix::config::checkout_options::Error),

	#[error("failed to materialize worktree: {0}")]
	Materialize(#[source] gix_worktree_state::checkout::Error),

	#[error("failed to open Arc-backed object database: {source}")]
	OpenArcObjects {
		#[source]
		source: io::Error,
	},

	#[error("failed to find tree entry at `{path}`: {source}")]
	TreeLookupEntry {
		path:   String,
		#[source]
		source: gix::object::find::existing::Error,
	},

	#[error("failed to find blob at `{path}`: {source}")]
	FindBlob {
		path:   String,
		#[source]
		source: gix::object::find::existing::with_conversion::Error,
	},

	#[error("failed to find tree at `{path}`: {source}")]
	FindTree {
		path:   String,
		#[source]
		source: gix::object::find::existing::with_conversion::Error,
	},

	#[error("failed to traverse tree: {source}")]
	TreeTraverse {
		#[source]
		source: gix::diff::object::decode::Error,
	},

	#[error("failed to lookup git HEAD: {source}")]
	Head {
		#[source]
		source: gix::reference::find::existing::Error,
	},

	#[error("failed to find git object: {source}")]
	ObjectLookup {
		#[source]
		source: gix::head::peel::to_object::Error,
	},

	#[error("failed to enumerate git references: {source}")]
	ReferencesOpen {
		#[source]
		source: gix::reference::iter::Error,
	},

	#[error("failed to iterate all git references: {source}")]
	ReferencesAll {
		#[source]
		source: gix::reference::iter::init::Error,
	},

	#[error("failed to peel git references: {source}")]
	ReferencesPeeled {
		#[source]
		source: gix_ref::packed::buffer::open::Error,
	},

	#[error("invalid utf-8 in blob at `{path}`: {source}")]
	BlobEncoding {
		path:   String,
		#[source]
		source: std::str::Utf8Error,
	},

	#[error("failed to parse `{path}` as TOML: {source}")]
	TomlParse {
		path:   String,
		#[source]
		source: toml::de::Error,
	},

	#[error("invalid version `{version}` in `{path}`: {source}")]
	VersionParse {
		path:    String,
		version: String,
		#[source]
		source:  semver::Error,
	},
}
