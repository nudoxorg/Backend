use crates_io_api::{CratesQuery, SyncClient};

use crate::traits::registry::Registry;

pub struct Crates {
    client: SyncClient,
}

pub struct Package {
    pub slug: String,
    pub name: String,
    pub language: Language,
    pub uuid: i64,
    pub source: Url,
}

impl From<Crate> for Package {
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
