//! A second fmt error conversion must not make derived CSS functions ambiguous.
extern crate self as style_traits;
use std::fmt::{self, Write};
use stylo_derive::ToCss;

pub struct CssWriter<W: Write>(W);
impl<W: Write> Write for CssWriter<W> {
    fn write_str(&mut self, value: &str) -> fmt::Result { self.0.write_str(value) }
}
pub trait ToCss {
    fn to_css<W: Write>(&self, dest: &mut CssWriter<W>) -> fmt::Result;
}
pub mod values {
    use super::*;
    pub struct SequenceWriter<'a, W: Write> { dest: &'a mut CssWriter<W>, separator: &'a str, wrote: bool }
    impl<'a, W: Write> SequenceWriter<'a, W> {
        pub fn new(dest: &'a mut CssWriter<W>, separator: &'a str) -> Self { Self { dest, separator, wrote: false } }
        pub fn item<T: ToCss>(&mut self, item: &T) -> fmt::Result {
            if self.wrote { self.dest.write_str(self.separator)?; }
            self.wrote = true;
            item.to_css(self.dest)
        }
    }
}
struct OtherError;
impl From<OtherError> for fmt::Error { fn from(_: OtherError) -> Self { fmt::Error } }
struct Number(i32);
impl ToCss for Number {
    fn to_css<W: Write>(&self, dest: &mut CssWriter<W>) -> fmt::Result { write!(dest, "{}", self.0) }
}
#[derive(ToCss)]
#[css(function)]
struct Coordinates(Number, Number);
#[derive(ToCss)]
enum Empty { #[css(function = "empty")] Value(#[css(skip)] bool) }

#[test]
fn css_functions_keep_their_serialization_with_another_error_conversion() {
    let mut output = CssWriter(String::new());
    Coordinates(Number(1), Number(2)).to_css(&mut output).unwrap();
    assert_eq!(output.0, "coordinates(1 2)");
    let mut empty = CssWriter(String::new());
    Empty::Value(false).to_css(&mut empty).unwrap();
    assert_eq!(empty.0, "empty()");
}
#[test]
fn css_functions_propagate_writer_failure() {
    struct Fails;
    impl Write for Fails { fn write_str(&mut self, _: &str) -> fmt::Result { Err(fmt::Error) } }
    assert!(Coordinates(Number(1), Number(2)).to_css(&mut CssWriter(Fails)).is_err());
}
