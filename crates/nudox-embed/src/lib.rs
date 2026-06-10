mod hash_utils;
pub mod in_process;
pub mod mock;
pub mod placeholder;
pub mod remote;

pub use in_process::InProcessEmbedder;
pub use mock::MockEmbedder;
pub use placeholder::PlaceholderEmbedder;
pub use remote::RemoteEmbedder;
