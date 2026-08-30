use nudox_id::ObjectDomain;
use nudox_root::ValidatedRoot;

fn escape(bytes: &[u8]) -> ValidatedRoot<'static, ObjectDomain> {
    ValidatedRoot::try_from(bytes).unwrap()
}
fn main() {}

