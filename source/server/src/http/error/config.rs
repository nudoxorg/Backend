use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
	#[error("`{name}`: invalid socket address: {source}")]
	AddrParse {
		name:   &'static str,
		#[source]
		source: std::net::AddrParseError,
	},

	#[error("`{name}`: invalid URL: {source}")]
	InvalidUrl {
		name:   &'static str,
		#[source]
		source: url::ParseError,
	},

	#[error("`{name}`: missing required environment variable")]
	MissingEnv { name: &'static str },

	#[error("`{name}`: invalid integer: {source}")]
	ParseInt {
		name:   &'static str,
		#[source]
		source: std::num::ParseIntError,
	},

	#[error("`{name}`: invalid bool: {source}")]
	ParseBool {
		name:   &'static str,
		#[source]
		source: std::str::ParseBoolError,
	},

	#[error("`{name}`: value is not valid UTF-8")]
	NotUnicode { name: &'static str },

	#[error("`{name}`: unsupported value `{value}`")]
	InvalidValue { name: &'static str, value: String },

	#[error("`{name}` must not be empty")]
	EmptyValue { name: &'static str },

	#[error("Qdrant vector search is not configured")]
	MissingQdrant,

	#[error("TerminusDB graph store is not configured")]
	MissingTerminus,

	#[error("provide either ?uri=<symbol-uri> or ?symbol=<fq_name>&language=<lang>")]
	MissingQueryParams,

	#[error("session value must not be empty")]
	EmptySession,
}
