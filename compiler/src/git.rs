use std::path::PathBuf;

use gix::{Repository, progress::Discard, remote};
use url::Url;

pub fn clone_repository(out_path: &PathBuf, remote: &Url) -> Repository {
	// Clone the repository
	let mut fetch_handle = gix::prepare_clone(remote.to_string(), &out_path)
		.unwrap()
		.with_fetch_options(remote::ref_map::Options::default());

	// Get the repository, ignoring the progress object, and monitoring for
	// interruptions, then stripping the returned Outcome object, taking only
	// repository.
	let (mut checkout_handle, _) =
		fetch_handle.fetch_then_checkout(Discard, &gix::interrupt::IS_INTERRUPTED).unwrap();

	// Checkout the tree into disk, again ignoring details for streamlined process
	checkout_handle.main_worktree(Discard, &gix::interrupt::IS_INTERRUPTED).unwrap().0
}
