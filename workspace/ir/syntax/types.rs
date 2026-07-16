#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ResolvedReference {
	pub target: crate::entry::NudoxPath,
	pub span:   std::ops::Range<usize>,
	pub kind:   ReferenceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(strum::FromRepr)]
#[repr(u8)]
pub enum ReferenceKind {
	FunctionCall  = 0,
	MethodCall    = 1,
	TypeReference = 2,
	VariableUse   = 3,
	MacroInvocation = 4,
	FieldAccess   = 5,
	Import        = 6,
}
