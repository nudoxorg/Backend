use std::time::Duration;

use crates_io_api::AsyncClient;
use lang_types::Language;

use crate::core::rust::Crates;

/// Construct the appropriate registry for the given language.
///
/// When additional languages are supported, this should be refactored into a
/// generic dispatch (e.g. a top-level match that invokes a generic pipeline)
/// since each language returns a distinct concrete type.
pub fn get_registry(language: Language) -> Crates {
	match language {
		Language::Rust => Crates {
			client: AsyncClient::new("my_bot (help@my_bot.com)", Duration::from_secs(1)).unwrap(),
		},
		_ => todo!("registry not yet implemented for {language:?}"),
	}
}
