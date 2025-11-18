use lang_types::Language;

use crate::traits::registry::Registry;

pub fn get_registry(language: Language) -> Registry {
    // There's a large set of languages we're yet to support unfortunately
    match language {
        Language::Rust => todo!(),
        other => todo!(),
    }
}
