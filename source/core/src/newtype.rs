/// Declare a validated, `#[serde(transparent)]` string-newtype.
///
/// Generated type has:
/// - Private `String` field (sealed; use `as_str()` / `From` impls).
/// - `From<String>` and `From<&str>` constructors (unvalidated, for internal
///   use).
/// - `new()` that rejects blank strings.
/// - `as_str()`, `into_string()`, `Display`, `FromStr`, `AsRef<str>`.
/// - `Hash + Eq + Ord` so it can be used directly as a `HashMap` key.
#[macro_export]
macro_rules! str_newtype {
    ($(#[$m:meta])* $vis:vis $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[derive(serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        $vis struct $name(String);

        impl $name {
            /// Construct from a non-blank string. Returns `None` if empty or whitespace-only.
            pub fn new(s: impl Into<String>) -> Option<Self> {
                let s = s.into();
                if s.trim().is_empty() { None } else { Some(Self(s)) }
            }

            pub fn as_str(&self) -> &str { &self.0 }

            pub fn into_string(self) -> String { self.0 }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self { Self(s) }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self { Self(s.to_owned()) }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::EmptyValue;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s).ok_or($crate::EmptyValue)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str { &self.0 }
        }
    };
}

/// Error returned when a newtype string is constructed from a blank value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyValue;

impl std::fmt::Display for EmptyValue {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str("value must be non-empty")
	}
}

impl std::error::Error for EmptyValue {}
