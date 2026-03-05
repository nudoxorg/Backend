use crates_io_api::CratesQuery;
use lang_types::Language;
use url::Url;

use crate::{core::rust::{self, Crates}, error::NewDocsError, traits::package};

pub trait Registry {
	type Error;

	/// Search all available packages for a specific query and return any hits.
	/// No hits are represented as an empty array.
	/// This operation is async and can fail due to network requests.
	async fn search_packages(
		&self,
		query: &str,
	) -> Result<Vec<Box<dyn package::Package>>, Self::Error>;

	/// Find a specific package by UUID.
	/// This operation is async and can fail (e.g., if the package is not found).
	async fn get_package_by_uuid(&self, uuid: u64) -> Result<Box<dyn package::Package>, Self::Error>;

	/// Find all packages that share a specific name.
	/// This operation is async and can fail.
	async fn get_packages_by_name(
		&self,
		name: &str,
	) -> Result<Vec<Box<dyn package::Package>>, Self::Error>;

	/// Return the package reflecting the language reference.
	/// This operation is not fallible as it largely fills in known information.
	async fn get_reference(&self) -> Box<dyn package::Package>;
}

pub enum AnyRegistry {
	Crates(Crates),
}

impl Registry for AnyRegistry {
	type Error = NewDocsError;

	async fn search_packages(
		&self,
		query: &str,
	) -> Result<Vec<Box<dyn package::Package>>, NewDocsError> {
		match self {
			AnyRegistry::Crates(r) => {
				let q = CratesQuery::builder().search(query).build();
				let result =
					r.client.crates(q).await.map_err(|e| NewDocsError::NetworkError(e.to_string()))?;
				Ok(
					result
						.crates
						.into_iter()
						.map(|c| Box::new(rust::Package::from(c)) as Box<dyn package::Package>)
						.collect(),
				)
			}
		}
	}

	async fn get_package_by_uuid(
		&self,
		uuid: u64,
	) -> Result<Box<dyn package::Package>, NewDocsError> {
		match self {
			AnyRegistry::Crates(r) => {
				let c = r
					.client
					.get_crate(&uuid.to_string())
					.await
					.map_err(|e| NewDocsError::NetworkError(e.to_string()))?;
				Ok(Box::new(rust::Package::from(c.crate_data)))
			}
		}
	}

	async fn get_packages_by_name(
		&self,
		name: &str,
	) -> Result<Vec<Box<dyn package::Package>>, NewDocsError> {
		match self {
			AnyRegistry::Crates(r) => {
				let c =
					r.client.get_crate(name).await.map_err(|e| NewDocsError::NetworkError(e.to_string()))?;
				Ok(vec![Box::new(rust::Package::from(c.crate_data))])
			}
		}
	}

	async fn get_reference(&self) -> Box<dyn package::Package> {
		match self {
			AnyRegistry::Crates(_) => Box::new(rust::Package {
				slug:        "rust-reference".into(),
				name:        "The Rust Reference".into(),
				language:    Language::Rust,
				uuid:        0,
				source:      Url::parse("https://doc.rust-lang.org/reference/").unwrap(),
				description: Some("The official reference manual for the Rust language".into()),
			}),
		}
	}
}
