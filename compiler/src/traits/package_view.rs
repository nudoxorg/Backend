use std::collections::HashMap;

use semver::Version;
use url::Url;

#[allow(dead_code)]
pub trait PackageView {
	// The name you'd expect to see it referred to as (Can just be a derivative of
	// the slug)
	fn name(&self) -> &str;

	// The battle-ready slug for encoding and references
	fn slug(&self) -> &str;

	// The precise version of the doc
	fn version(&self) -> &Version;

	// Any extraneous links like the source page, or the projects home
	fn links(&self) -> &HashMap<String, Url>;

	// Builds the pages and sends them off to mongo?
	async fn build_pages(&self) -> Result<(), anyhow::Error>;
}
