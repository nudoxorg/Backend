use crate::index::{EntryIndex, Indexable, UntypedEntryIndex};

// derive macro re-export
pub(crate) use self::m::Visitor;

pub(crate) trait Visitor {
    #[expect(dead_code)]
    fn visit(&self, f: &impl Fn(UntypedEntryIndex));

    fn visit_mut(&mut self, f: &impl Fn(&mut UntypedEntryIndex));
}

impl<T: Indexable> Visitor for EntryIndex<T> {
    fn visit(&self, f: &impl Fn(UntypedEntryIndex)) {
        f(self.raw());
    }

    fn visit_mut(&mut self, f: &impl Fn(&mut UntypedEntryIndex)) {
        f(self.cast_mut());
    }
}

mod default_impl {
    use std::{num::NonZeroU16, ops::Range, path::PathBuf};

    use super::*;

    impl<T: Visitor> Visitor for Option<T> {
        fn visit(&self, f: &impl Fn(UntypedEntryIndex)) {
            if let Some(it) = self {
                it.visit(f);
            }
        }

        fn visit_mut(&mut self, f: &impl Fn(&mut UntypedEntryIndex)) {
            if let Some(it) = self {
                it.visit_mut(f);
            }
        }
    }

    impl<T: Visitor + ?Sized> Visitor for Box<T> {
        fn visit(&self, f: &impl Fn(UntypedEntryIndex)) {
            (**self).visit(f);
        }

        fn visit_mut(&mut self, f: &impl Fn(&mut UntypedEntryIndex)) {
            (**self).visit_mut(f);
        }
    }

    impl<T: Visitor> Visitor for [T] {
        fn visit(&self, f: &impl Fn(UntypedEntryIndex)) {
            for it in self {
                it.visit(f);
            }
        }

        fn visit_mut(&mut self, f: &impl Fn(&mut UntypedEntryIndex)) {
            for it in self {
                it.visit_mut(f);
            }
        }
    }

    // standard integer types
    m::__default_impl_visitor!(u8, u16, u32, u64, u128, usize);
    m::__default_impl_visitor!(i8, i16, i32, i64, i128, isize);

    // other primitive types
    m::__default_impl_visitor!((), bool, char);

    // std types (as needed)
    m::__default_impl_visitor!(String, PathBuf, NonZeroU16, Range<usize>);
}

mod m {
    use super::*;

    /// derive macro to implement Visitor pattern recursively
    pub(crate) macro Visitor {
		// special-case `struct Struct;`: gets a no-op visitor impl
	    derive() (
	    	$(#[$meta:meta])*
	    	$vis:vis struct $ident:ident;
	    ) => {
	    	__default_impl_visitor!($ident);
	    },

	    // typical struct, calls `.visit` for all fields
		derive() (
	    	$(#[$meta:meta])*
	    	$vis:vis struct $ident:ident {
				$(
				$(#[$m:meta])*
				$v:vis $field:ident: $ty:ty
				),* $(,)?
		    }
		) => {
	    	#[automatically_derived]
	    	impl Visitor for $ident
			{
				fn visit(&self, f: &impl Fn(UntypedEntryIndex)) {
					$(
					self.$field.visit(f);
					)*
				}

				fn visit_mut(&mut self, f: &impl Fn(&mut UntypedEntryIndex)) {
					$(
					self.$field.visit_mut(f);
					)*
				}
	    	}
	    },

	    // parsing enums is much more compilcated, due to the different variants,
	    // so we use a TT muncher to build up match $pat-$(expr)* pairs to visit
	    // all children of each enum variant.
	    //
	    // implementation done through the __derive_enum_impl macro below to not
	    // expose the TT munching through the public API (not that it would ever
	    // get used, since this is a derive macro...)
	    derive() (
	    	$(#[$meta:meta])*
	    	$vis:vis enum $ident:ident {
		    	$($variants:tt)*
	    	}
	    ) => {
	    	__derive_enum_impl!($ident $($variants)*);
	    },
	}

    /// internal macro to TT-munch parse enum variants into a representation
    /// that we can use to emit a recursive `Visitor` implementation
    #[doc(hidden)]
    pub(crate) macro __derive_enum_impl {
		// special-case `enum Enum {}`: gets a no-op visitor impl
        ($ident:ident) => {
            __default_impl_visitor!($ident);
        },

        // typical enum, starts the munching process
		($ident:ident $($tt:tt)*) => {
			__derive_enum_impl!(@munch $ident [] $($tt)*);
		},

		// base case, no variants left -> actually emit the impl with the
		// @derive branch of our macro, passing in the accumumlated pairs
		(@munch
			$ident:ident
			[$($acc:tt)*]
		) => {
			__derive_enum_impl!(@derive $ident [$($acc)*]);
		},

        // struct variant
        (@munch
        	$ident:ident
          	[$($acc:tt)*]

          	$(#[$meta:meta])*
          	$variant:ident {
          		$($inner:ident: $ty:ty),* $(,)?
          	},

          	$($rest:tt)*
        ) => {
        	__derive_enum_impl!(@munch
          		$ident
          		// matcher matches each inner variable, and outputs the exprs
          		// as the as-is ident that is bound in the matcher.
          		[$($acc)* [Self::$variant { $($inner),* }] [$($inner),*]]
          		$($rest)*
          	);
    	},

    	// tuple variant
    	//
    	// generate each binding ident once and reuse the same token in both
    	// the pattern and the expression list so they share hygiene.
    	//
    	// we pass a list of index literals to `@tuple_indices`, which
    	// materialises binding idents using `${concat(_, $idx)}`
    	// (which needs a literal metavariable, not `${index()}`).
    	(@munch
    		$ident:ident
    		[$($acc:tt)*]

          	$(#[$meta:meta])*
          	$variant:ident($($ty:ty),* $(,)?),

    		$($rest:tt)*
    	) => {
          	__derive_enum_impl!(@tuple_indices
          		$ident
          		[$($acc)*]
          		$variant
          		// ${ignore($ty)} to hint to ${index()} which variable
          		// we want the index of
          		($(${ignore($ty)} ${index()}),*)
          		$($rest)*
          	);
    	},

    	// turns index literals into binding identifiers (once each),
    	// then pass through to @tuple_emit to actually build the pair
    	(@tuple_indices
    		$ident:ident
    		[$($acc:tt)*]
    		$variant:ident
    		($($idx:literal),*)
    		$($rest:tt)*
    	) => {
          	__derive_enum_impl!(@tuple_emit
          		$ident
          		[$($acc)*]
          		$variant
          		// create the identifier based on the literal index here
          		($(${concat(_, $idx)}),*)
          		$($rest)*
          	);
    	},

    	// @tuple_emit: reuses the `$binding` token in pattern AND expr
    	(@tuple_emit
    		$ident:ident
    		[$($acc:tt)*]
    		$variant:ident
    		($($binding:ident),*)
    		$($rest:tt)*
    	) => {
          	__derive_enum_impl!(@munch
          		$ident
          		[
          			$($acc)*
          			[Self::$variant($($binding),*)]
          			[$($binding),*]
          		]
          		$($rest)*
          	);
    	},

        // no variant associated data, emit a matcher without
        // any actual expressions (there's nothing to visit)
		(@munch
        	$ident:ident
          	[$($acc:tt)*]

          	$(#[$meta:meta])*
          	$variant:ident,
          	$($rest:tt)*
    	) => {
          	__derive_enum_impl!(@munch
          		$ident
          		[$($acc)* [Self::$variant] []]
          		$($rest)*
          	);
        },

        // emits the actual implementation, using the pat/$(expr)* pairs
        // to visit all the children of the enum variants.
	    (@derive $ident:ident [$([$variant:pat] [$($expr:expr),*])+]) => {
	    	#[automatically_derived]
	    	impl Visitor for $ident
	    	{
	    		fn visit(&self, f: &impl Fn(UntypedEntryIndex)) {
	    			match self {
	    				$(
	    				$variant => {
	    					$(
	    					$expr.visit(f);
		    				)*
	    				}
		    			)*
	    			}
	    		}

	    		fn visit_mut(&mut self, f: &impl Fn(&mut UntypedEntryIndex)) {
	    			match self {
	    				$(
	    				$variant => {
	    					$(
	    					$expr.visit_mut(f);
		    				)*
	    				}
		    			)*
	    			}
	    		}
	    	}
	    },
    }

    #[doc(hidden)]
    pub(crate) macro __default_impl_visitor {
	    ($ty:ty) => {
	        impl $crate::visitor::Visitor for $ty {
	            fn visit(&self, _: &impl Fn($crate::index::UntypedEntryIndex)) {}
	            fn visit_mut(&mut self, _: &impl Fn(&mut $crate::index::UntypedEntryIndex)) {}
	        }
	    },
		($($ty:ty),*) => {
			$(
			__default_impl_visitor!($ty);
			)*
		},
	}
}
