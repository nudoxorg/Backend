#[link(name = "private")]
unsafe extern "C" { fn fixture_import() -> u32; }
#[unsafe(no_mangle)]
pub extern "C" fn fixture_export() -> u32 { 42 }
fn main() { println!("{}", unsafe { fixture_import() } + fixture_export()); }
