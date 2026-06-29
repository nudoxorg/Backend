//! The Server — the layer responsible for coordinating updates between
//! subsystems, and the main (only) entrypoint for requests.

pub mod coordination;
pub mod http;
pub mod search;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use url::Url;

use registry::index::GlobalStore;
use runtime::graph::Graph;
use runtime::vector::Semantic;

/// Our server configuration
/// This is not meant to be constructed in any other way besides "default", as this is a private-facing configuration, where for simplicity we want static defaults
pub struct ServerConfiguration {
	serving_address: SocketAddr,
	remote_dependencies: HashMap<String, Url>,
	// TODO: Add fields for the rest of the configuration for these databases or simply scope it in the function bodies? Don't see a reason to carry most of this data around
}

impl Default for ServerConfiguration {
	fn default() -> Self {
		let serving_address = SocketAddr::from(([127, 0, 0, 1], 1000));

		// We assign them here as untyped key/value pairs, typing them through the methods
		// TODO: Write as an enum?
		        let remote_dependencies = HashMap::from([(
            "terminus".to_owned(),
            Url::parse("http://127.0.0.1:1000").expect("static URL is valid"),
        )]);

		Self {
			serving_address,
			remote_dependencies,
		}
	}
}

impl ServerConfiguration {
	/// The amount of time we're going to wait before we give up on uploading a document
	const UPLOAD_TIMEOUT:
    std::time::Duration = Duration::from_millis(2000);

    fn terminus_endpoint(&self) -> Option<&Url> {
        self.remote_dependencies.get("terminus")
    }
}

//! Also I'm not messing with any of the env stuff. I think we'll be able to get with configuration flags for most of this, just having separate default implementation/etc for dev/prod/other

pub struct Server {
	// TODO: Figure out connection types
	pub graph: Graph,
	pub semantics: Semantic,
	pub global_store: GlobalStore,
}

impl Server {

}
