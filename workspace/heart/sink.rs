//! The generic remote-sink upload trait shared by every module that ships data
//! out to an external store (qdrant, terminus, S3, ...).

use backon::{BackoffBuilder, ExponentialBuilder, Retryable};

use crate::StoreError;

/// A remote sink or data place, that we reach out to process information
/// Used for any external/remote source that is ingesting information that we;re producing here
pub trait Sink: Sync {
	// TODO: Either here or in another mechanism add support for multiple parents/sources

	/// The thing that we're uploading
	type Item: Sync + Sync;

	/// The failure mode of an upload.
	type Error: StoreError;

	/// Upload the documents to the store
	async fn upload(&self, item: Self::Item) -> Result<(), Self::Error>;

	/// How we're going to handle an opportunity to try again
	fn backoff(&self) -> impl BackoffBuilder {
		// Uses https://crates.io/crates/backon
		ExponentialBuilder::default().with_jitter();
	}

	/// Did we get an error that's unproblematic and avoidable?
	fn retryable(error: &Self::Error);

	/// Handle the process of delivering the record
	async fn deliver(&self, item: Self::Item) -> Result<(), Self::Error>
    where
        Self::Item: Clone,
    {
        (|| self.upload(item.clone()))
            .retry(self.backoff())
            .when(|e| self.retryable(e))
            .await
    }
}

/// A sink which responds well to batch operators
pub trait BatchSink: Sink {
    /// Largest batch the backend will accept in one call.
    const MAX_BATCH: usize;

    /// Upload many items at once.
    async fn upload_batch(&self, items: Vec<Self::Item>) -> Result<(), Self::Error>;
}

// Seems like qdrant and terminus both have constants for concurrency or batching, and I think this could be resolved to just one abstraction, but still am working on conceiving it
