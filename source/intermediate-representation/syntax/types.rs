#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedReference {
	pub target: crate::entry::NudoxPath,
	pub span:   std::ops::Range<usize>,
	pub kind:   ReferenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReferenceKind {
	FunctionCall,
	MethodCall,
	TypeReference,
	VariableUse,
	MacroInvocation,
	FieldAccess,
	Import,
}
