//! Secondary fixture module for cross-file declaration and re-export proof.
//! It contains the declaration re-exported by `lib.rs`.
//! No external crate is required.

/// Adds a [`Reexported`] value while preserving `Value`.
pub fn documented<Value>(value: Value, величина: usize) -> Value
where
    Value: Clone,
{
    if величина == 0 {
        return value;
    }
    let record = Reexported { value: 1 };
    if record.value == 0 {
        value
    } else {
        helper(value)
    }
}

fn helper<Value>(value: Value) -> Value {
    value
}

/// A named record.
pub struct Named {
    field: u32,
    label: &'static str,
}

pub struct Tuple(u8, u16);
pub struct UnitStruct;
pub enum Unit {
    Value,
}

pub enum Choice {
    First,
    Second = 7,
}

pub union Raw {
    integer: u32,
    bytes: [u8; 4],
}

pub trait Parent {}
pub trait LocalTrait: Parent {}
pub unsafe trait UnsafeTrait {}
pub auto trait AutoTrait {}
pub enum Demo {
    Value,
}
impl LocalTrait for Demo {}
impl Demo {
    pub fn inherent(&self) {}
}

pub fn tuple_parameter(value: (u8, u16)) -> (u8, u16) { value }
pub fn bounded<T: Into<Vec<u8>>>(value: T) -> T { value }
pub extern "C" fn c_abi() {}

pub mod inner {
    pub fn f() {}
}

pub type Number = u64;
pub const ANSWER: u32 = 42;
pub static mut COUNTER: u32 = 0;

pub async fn asynchronous() {}
pub unsafe fn dangerous() {}
pub const fn constant() -> usize {
    1
}

/// A declaration from another source file.
pub struct Reexported {
    value: usize,
}
