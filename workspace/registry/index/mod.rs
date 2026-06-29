//! This provides our main interface for creating an index over all of the
//! parsed packages, and giving everything a global identification.
//!
//! Each item not only provides the mean to retrieve itself from the store, but
//! also it's status: Which stage it's in (if not complete), what dependents it
//! needs to be complete, and whether or not it's loaded into the runtime stores
//! and systems.

pub enum ResolutionState {
	/// No indexing has been attempted yet
	Unindexed {
		/// Indexing for a package which is related to this has been accomplished, and is incomplete with this package left untouched
	 needed: bool
	},
	/// The indexing is in progress, and will soon be ready to work with.
	Progressing(Phase),
	/// All phases complete, stored and actionable
	Stored
}

/// The phase that the indexing is currently in, and the particular progress that that phase has gone through
pub enum Phase {
	/// The compiler is chewing through it
	Compiling,
	/// Treesitter has chewed through the library and produced a concrete tree
	Treesat

	// I don't know the rest of this
}


//! The concept of a resolution outcome is frankly pointless, A package always holds a state, it's not something with an "outcome" if we need to re-index, then we change the state. The outcome is just weird and voltaile.


// TODO: Wire up postgres types for global indexing, and the parse queue and association (establishing a link)

/// Our globalstore/connective tissue (postgres)
pub struct GlobalStore {

}
