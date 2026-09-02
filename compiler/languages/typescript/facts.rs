//! Owned, arena-independent TypeScript extraction facts.
//! These records are the stable boundary between OXC parsing and consumers.
//! They deliberately contain no OXC types or arena lifetimes.
#![allow(
    missing_docs,
    reason = "the fact vocabulary is documented by its stable record names"
)]

use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Facts<Element> {
    values: Box<[Element]>,
}
impl<Element> From<Vec<Element>> for Facts<Element> {
    fn from(value: Vec<Element>) -> Self {
        Self {
            values: value.into_boxed_slice(),
        }
    }
}
impl<Element> Facts<Element> {
    #[must_use]
    pub fn first(&self) -> Option<&Element> {
        self.values.first()
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, Element> {
        self.values.iter()
    }
}
impl<'a, Element> IntoIterator for &'a Facts<Element> {
    type Item = &'a Element;
    type IntoIter = std::slice::Iter<'a, Element>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}
impl<Element> IntoIterator for Facts<Element> {
    type Item = Element;
    type IntoIter = std::vec::IntoIter<Element>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.into_vec().into_iter()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Text {
    value: String,
}
impl From<String> for Text {
    fn from(value: String) -> Self {
        Self { value }
    }
}
impl From<&str> for Text {
    fn from(value: &str) -> Self {
        Self {
            value: value.to_owned(),
        }
    }
}
impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        &self.value
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bytes {
    value: std::collections::VecDeque<u8>,
}
impl From<Vec<u8>> for Bytes {
    fn from(value: Vec<u8>) -> Self {
        Self {
            value: value.into(),
        }
    }
}
impl Bytes {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceSpan {
    pub start: ByteOffset,
    pub end: ByteOffset,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ByteOffset {
    value: u32,
}

/// A declaration overload cardinality.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OverloadCount(u32);

impl OverloadCount {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
    #[must_use]
    pub const fn into_u32(self) -> u32 {
        self.0
    }
}

impl SourceSpan {
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self {
            start: ByteOffset { value: start },
            end: ByteOffset { value: end },
        }
    }
    #[must_use]
    pub const fn try_new(start: u32, end: u32) -> Option<Self> {
        if start <= end {
            Some(Self {
                start: ByteOffset { value: start },
                end: ByteOffset { value: end },
            })
        } else {
            None
        }
    }
}

impl ByteOffset {
    #[must_use]
    pub const fn into_u32(self) -> u32 {
        self.value
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleFacts {
    pub path: PathBuf,
    pub span: SourceSpan,
    pub declarations: Facts<DeclarationFact>,
    pub imports: Facts<ImportFact>,
    pub exports: Facts<ExportFact>,
    pub type_facts: Facts<TypeFact>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DeclarationKind {
    Interface,
    Class,
    TypeAlias,
    Enum,
    EnumMember,
    Namespace,
    Function,
    Const,
    Reexport,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationFact {
    pub kind: DeclarationKind,
    pub name: Text,
    pub span: SourceSpan,
    pub members: Facts<MemberFact>,
    pub overload_count: OverloadCount,
    pub type_parameters: Facts<Text>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemberModifiers {
    pub optional: ModifierPresence,
    pub readonly: ModifierPresence,
    pub definite: ModifierPresence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModifierPresence {
    Absent,
    Present,
}

impl From<bool> for ModifierPresence {
    fn from(value: bool) -> Self {
        if value { Self::Present } else { Self::Absent }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberFact {
    pub name: Text,
    pub kind: MemberKind,
    pub span: SourceSpan,
    pub modifiers: MemberModifiers,
    pub overload_count: OverloadCount,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberKind {
    Property,
    Method,
    Constructor,
    IndexSignature,
    ConstructSignature,
    Accessor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportFact {
    pub request: Text,
    pub name: ImportName,
    pub is_type: ModifierPresence,
    pub span: SourceSpan,
    pub resolution: ImportResolution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportName {
    Named { imported: String, local: String },
    Default,
    Namespace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportResolution {
    Resolved(PathBuf),
    UnresolvableImport { request: String, from: PathBuf },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportFact {
    pub name: Text,
    pub shape: ExportShape,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportShape {
    Named {
        local: String,
        overload_count: OverloadCount,
    },
    NamespaceOf(String),
    Star {
        request: String,
        alias: Option<String>,
    },
    Default,
    Unresolvable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeFact {
    Nominal {
        name: String,
        declaration: String,
    },
    TypeVar(String),
    Generic {
        base: String,
        arguments: Facts<TypeFact>,
    },
    Union(Box<[TypeFact]>),
    Intersection(Box<[TypeFact]>),
    Literal(String),
    Function(String),
    Dynamic(String),
    NoIrRepresentation {
        spelling: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractionError {
    #[error("parse error in {path:?} at {span:?}: {cause}")]
    Parse {
        path: PathBuf,
        span: SourceSpan,
        cause: ParseCause,
        offending: Bytes,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ParseCause {
    #[error("syntax diagnostic {code}: {message}")]
    Syntax { code: String, message: String },
}
