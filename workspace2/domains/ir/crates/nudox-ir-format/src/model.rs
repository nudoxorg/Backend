use nudox_ir_vocab::TypeId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimitiveType {
    Bool,
    I32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeNode {
    Primitive(PrimitiveType),
    Reference(TypeId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeNodeFault {
    Reserved { actual: [u8; 3] },
    Tag { actual: u8 },
    Primitive { actual: u32 },
    Edge { target: TypeId, node_count: u32 },
}

impl PrimitiveType {
    pub(crate) const fn code(self) -> u32 {
        match self {
            Self::Bool => 0,
            Self::I32 => 1,
        }
    }

    pub(crate) const fn decode(actual: u32) -> Result<Self, TypeNodeFault> {
        match actual {
            0 => Ok(Self::Bool),
            1 => Ok(Self::I32),
            actual => Err(TypeNodeFault::Primitive { actual }),
        }
    }
}
