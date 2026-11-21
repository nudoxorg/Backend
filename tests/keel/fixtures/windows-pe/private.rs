#[link(name = "leaf")]
unsafe extern "C" { fn fixture_leaf() -> u32; }
#[unsafe(no_mangle)]
pub extern "C" fn fixture_import() -> u32 { unsafe { fixture_leaf() } }
