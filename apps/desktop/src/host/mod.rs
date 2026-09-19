//! Process lifecycle: workspace paths, the owner lease, and the first frame.
//! One rule governs this module: a live owner always wins over a new one.
//! Nothing here renders; it only decides which service this window talks to.

pub(crate) mod launch;
pub(crate) mod lease;
pub(crate) mod paths;
