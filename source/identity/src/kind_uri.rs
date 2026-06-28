use std::fmt;

/// Canonical `{prefix}/{lang}/{package}/{path}` URI for Kind documents stored
/// in TerminusDB. The prefix is the schema class name (e.g. `"Function"`,
/// `"RecordType"`), mirroring the `Entry/…` scheme used by [`EntryUri`].
///
/// [`EntryUri`]: crate::EntryUri
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KindUri {
	prefix:  &'static str,
	lang:    String,
	package: String,
	path:    String,
}

impl KindUri {
	pub fn new(prefix: &'static str, lang: &str, package: &str, path: &str) -> Self {
		Self { prefix, lang: lang.to_owned(), package: package.to_owned(), path: path.to_owned() }
	}
}

impl fmt::Display for KindUri {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}/{}/{}/{}", self.prefix, self.lang, self.package, self.path)
	}
}

impl From<KindUri> for String {
	fn from(u: KindUri) -> String { u.to_string() }
}
