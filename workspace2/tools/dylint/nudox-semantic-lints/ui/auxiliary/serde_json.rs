pub struct Value;

#[macro_export]
macro_rules! json {
    ($($value:tt)*) => {{ $crate::Value }};
}
