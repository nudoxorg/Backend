#![allow(
    clippy::large_enum_variant,
    clippy::result_large_err,
    reason = "adapter tests retain exact scenario and SDK failures without heap-erasing sources"
)]

mod overload;
mod signal_contract;
mod support;
