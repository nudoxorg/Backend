use thiserror::Error;

#[derive(Debug, Error)]
pub enum TerminusError {
	#[error("failed to create TerminusDB client for `{org}/{db}`")]
	ClientCreation {
		org:    String,
		db:     String,
		#[source]
		source: anyhow::Error,
	},

	#[error("failed to upload schema to TerminusDB")]
	SchemaUpload {
		#[source]
		source: anyhow::Error,
	},

	#[error("failed to upload documents to TerminusDB")]
	DocumentUpload {
		#[source]
		source: anyhow::Error,
	},

	#[error("failed to fetch document `{uri}` from TerminusDB")]
	DocumentFetch {
		uri:    String,
		#[source]
		source: anyhow::Error,
	},

	#[error("failed to build TerminusDB HTTP client")]
	HttpClient {
		#[source]
		source: anyhow::Error,
	},
}
