// Convention: fields typed `Option<Vec<T>>` distinguish *absent* (`null` in JSON)
// from *empty* (`[]`). Callers must double-unwrap; this is intentional because
// the distinction carries semantic meaning (e.g. "no parameters" vs "zero parameters").
pub mod entry;
pub mod function;
pub mod generics;
pub mod kind;
pub mod module;
pub mod parameter;
pub mod primitives;
pub mod protocols;
pub mod record;
pub mod syntax;
pub mod ty;
pub mod pipeline;
