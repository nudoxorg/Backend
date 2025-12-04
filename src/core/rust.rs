use crate::error::NewDocsError;
use crate::traits::{package::Package, registry::Registry};
use crates_io_api::{Crate, CratesQuery, SyncClient};
use lang_types::Language;
use url::Url;

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

// Removed duplicate `From<Crate>` implementation - kept the more complete one
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

    fn dependencies(&self) -> Result<Vec<Box<dyn Package>>, NewDocsError> {
        todo!()
    }
}

impl Registry for Crates {
    type Error = NewDocsError;

    async fn search_packages(
        &self,
        query: &str,
    ) -> Result<Vec<Box<dyn crate::traits::package::Package>>, NewDocsError> {
        let query = CratesQuery::builder().search(query).build();
        let result = self.client.crates(query)?;

        // Fixed: convert Crate to RPackage, box it, and collect
        Ok(result
            .crates
            .into_iter()
            .map(|c| Box::new(RPackage::from(c)) as Box<dyn Package>)
            .collect())
    }

    async fn get_package_by_uuid(
        &self,
        uuid: u64,
    ) -> Result<Box<dyn crate::traits::package::Package>, NewDocsError> {
        todo!()
    }

    async fn get_packages_by_name(
        &self,
        name: &str,
    ) -> Result<Vec<Box<dyn crate::traits::package::Package>>, NewDocsError> {
        todo!()
    }

    async fn get_reference(&self) -> Box<dyn crate::traits::package::Package> {
        todo!()
    }
}
