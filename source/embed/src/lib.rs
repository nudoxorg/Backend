mod hash_utils;
pub mod in_process;
pub mod placeholder;
pub mod remote;
pub mod test_mock;

pub use in_process::InProcessEmbedder;
pub use placeholder::PlaceholderEmbedder;
pub use remote::RemoteEmbedder;
pub use test_mock::MockEmbedder;
