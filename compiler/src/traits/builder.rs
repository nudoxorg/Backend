use std::time::Duration;

use crates_io_api::{AsyncClient, SyncClient};
use lang_types::Language;

use crate::{core::rust::Crates, traits::{package::Package, registry::Registry}};

pub fn get_registry(language: Language) -> impl Registry {
	// There's a large set of languages we're yet to support unfortunately
	match language {
		Language::Rust => Crates {
			client: AsyncClient::new("my_bot (help@my_bot.com)", Duration::from_secs(1)).unwrap(),
		},
		_ => todo!(),
	}
}
