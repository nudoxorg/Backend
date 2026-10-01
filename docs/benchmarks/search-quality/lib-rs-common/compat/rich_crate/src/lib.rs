//! Minimal compile-time Origin shim matching the pinned upstream rich_crate API
//! used by search_index. The benchmark inputs use only CratesIo origins.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Origin {
    CratesIo(Box<str>),
}

impl Origin {
    pub fn from_crates_io_name(name: &str) -> Self {
        Self::CratesIo(name.to_ascii_lowercase().into())
    }

    pub fn from_str(value: impl AsRef<str>) -> Self {
        let value = value.as_ref();
        let name = value.strip_prefix("crates.io:").expect("crates.io origin");
        Self::from_crates_io_name(name)
    }

    pub fn to_str(&self) -> String {
        match self {
            Self::CratesIo(name) => format!("crates.io:{name}"),
        }
    }

    pub fn is_crates_io(&self) -> bool {
        matches!(self, Self::CratesIo(_))
    }
}
