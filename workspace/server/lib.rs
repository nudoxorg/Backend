//! The Server — the layer responsible for coordinating updates between
//! subsystems, and the main (only) entrypoint for requests.

pub mod coordination;
pub mod http;
pub mod search;

use std::{collections::HashMap, net::SocketAddr, time::Duration};

use heart::Live;
use registry::index::GlobalStore;
use runtime::{graph::Graph, vector::Semantic};
use url::Url;

/// Our server configuration
/// This is not meant to be constructed in any other way besides "default", as
/// this is a private-facing configuration, where for simplicity we want static
/// defaults
pub struct ServerConfiguration {
	serving_address:     SocketAddr,
	remote_dependencies: HashMap<String, Url>,
}

impl Default for ServerConfiguration {
	fn default() -> Self {
		let serving_address = SocketAddr::from(([127, 0, 0, 1], 1000));

		// We assign them here as untyped key/value pairs, typing them through the
		// methods TODO: Write as an enum?
		let remote_dependencies = HashMap::from([(
			"terminus".to_owned(),
			Url::parse("http://127.0.0.1:1000").expect("static URL is valid"),
		)]);

		Self { serving_address, remote_dependencies }
	}
}

impl ServerConfiguration {
	/// The amount of time we're going to wait before we give up on uploading a
	/// document
	pub const UPLOAD_TIMEOUT: std::time::Duration = Duration::from_millis(2000);

	fn terminus_endpoint(&self) -> Option<&Url> { self.remote_dependencies.get("terminus") }

	/// Assemble to a server from already-connected (`Live`) stores.
	pub fn to_server(
		self,
		graph: Graph<Live>,
		semantics: Semantic<DIM, Live>,
		global_store: GlobalStore<Live>,
	) -> Self {
		let config = self;

		Self { config, graph, semantics, global_store }
	}
}

// Also I'm not messing with any of the env stuff. I think we'll be able to get
// with configuration flags for most of this, just having separate default
// implementation/etc for dev/prod/other

pub struct Server<const DIM: usize> {
	/// The static, private-facing configuration this server was built from.
	pub config: ServerConfiguration,

	/// Our graph database of choice (terminus).
	pub graph: Graph<Live>,

	/// Our semantic/vector embedding database of choice (Qdrant).
	pub semantics: Semantic<DIM, Live>,

	/// Our globalstore/connective tissue (postgres).
	pub global_store: GlobalStore<Live>,
}
