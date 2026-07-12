#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ResolvedReference {
	pub target: crate::entry::NudoxPath,
	pub span:   std::ops::Range<usize>,
	pub kind:   ReferenceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ReferenceKind {
	FunctionCall,
	MethodCall,
	TypeReference,
	VariableUse,
	MacroInvocation,
	FieldAccess,
	Import,
}
