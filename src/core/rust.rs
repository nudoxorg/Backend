use crates_io_api::{Crate, CratesQuery, SyncClient};
use lang_types::Language;
use url::Url;

use crate::traits::{package::Package, registry::Registry};

pub struct Crates {
    client: SyncClient,
}

pub struct RPackage {
    pub slug: String,
    pub name: String,
    pub language: Language,
    pub uuid: i64,
    pub source: Url,
}

impl From<Crate> for RPackage {
    fn from(crate_data: Crate) -> Self {
        let slug = crate_data.name.to_lowercase().replace(' ', "-");

        let source_url = crate_data
            .repository
            .and_then(|repo| Url::parse(&repo).ok())
            .unwrap_or_else(|| {
                // Fallback.
                Url::parse(&format!("https://crates.io/crates/{}", crate_data.name))
                    .expect("Failed to parse default URL")
            });

        let uuid = 0;

        RPackage {
            slug,
            name: crate_data.name,
            language: Language::Rust,
            uuid,
            source: source_url,
        }
    }
}

impl Package for RPackage {
    fn get_available_versions(&self) -> Result<Vec<semver::Version>, NewDocsError> {
        todo!()
    }

    fn flags(&self) -> Result<Option<Vec<String>>, NewDocsError> {
        todo!()
    }

    fn description(&self) -> Result<Option<String>, NewDocsError> {
        todo!()
    }

    fn dependencies(&self) -> Result<Vec<Box<dyn Package>>, NewDocsError> {
        todo!()
    }

    fn dependents(&self) -> Result<Vec<Box<dyn Package>>, NewDocsError> {
        todo!()
    }

    fn retrieve(
        &self,
        version: semver::Version,
        flags: Option<Vec<String>>,
    ) -> Result<String, NewDocsError> {
        todo!()
    }
}

impl Registry for Crates {
    async fn search_packages(
        &self,
        query: &str,
    ) -> Result<Vec<Box<dyn crate::traits::package::Package>>, PackageRegistryError> {
        let query = CratesQuery::builder().search(query).build();
        let result = self.client.crates(query)?;

        result.crates

        // Frankly do not care about multi-page results right now
    }

    async fn get_package_by_uuid(
        &self,
        uuid: u64,
    ) -> Result<Box<dyn crate::traits::package::Package>, PackageRegistryError> {
        todo!()
    }

    async fn get_packages_by_name(
        &self,
        name: &str,
    ) -> Result<Vec<Box<dyn crate::traits::package::Package>>, PackageRegistryError> {
        todo!()
    }

    async fn get_reference(&self) -> Box<dyn crate::traits::package::Package> {
        todo!()
    }
}
