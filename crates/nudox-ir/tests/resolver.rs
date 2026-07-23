use serde::Deserialize;
use vfs::{
    async_vfs::{AsyncMemoryFS, AsyncVfsPath},
    error::VfsErrorKind,
};

use nudox_ir::prelude::*;

pub struct ExampleResolver {
    fs: AsyncVfsPath,
}

impl ExampleResolver {
    pub fn new() -> Self {
        ExampleResolver {
            fs: AsyncVfsPath::new(AsyncMemoryFS::new()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResolverError {
    #[error("vfs error: {0}")]
    IO(#[from] vfs::VfsError),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("entry not found")]
    MissingEntry(UniqueId<usize>),

    #[error("package not found")]
    MissingPackage(PackageId),
}

impl RegistryResolver for ExampleResolver {
    type EntryId = usize;

    type Error = ResolverError;

    async fn load_unique_id(&self, id: &UniqueId<Self::EntryId>) -> Result<Entry, Self::Error> {
        let package = match id.package().view() {
            PackageIdView::Path(path) => format!("{}", path.display()),
        };

        let entry = match id.entry() {
            Some(entry) => format!("{entry}"),
            None => String::from("root"),
        };

        let file = self.fs.join(package)?.join(entry)?.read_to_string().await;

        let file = match file {
            Ok(file) => file,
            Err(err) if matches!(err.kind(), VfsErrorKind::FileNotFound) => {
                return Err(ResolverError::MissingEntry(UniqueId::clone(id)));
            }
            Err(err) => return Err(ResolverError::IO(err)),
        };

        let deserializer = &mut serde_json::Deserializer::from_str(&file);

        let entry = Entry::deserialize(deserializer)?;

        Ok(entry)
    }

    async fn load_package_info(
        &self,
        id: PackageId,
    ) -> Result<PackageInfo<Self::EntryId>, Self::Error> {
        let package = match id.view() {
            PackageIdView::Path(path) => format!("{}", path.display()),
        };

        let file = self.fs.join(package)?.join("info")?.read_to_string().await;

        let file = match file {
            Ok(file) => file,
            Err(err) if matches!(err.kind(), VfsErrorKind::FileNotFound) => {
                return Err(ResolverError::MissingPackage(id));
            }
            Err(err) => return Err(ResolverError::IO(err)),
        };

        let deserializer = &mut serde_json::Deserializer::from_str(&file);

        let info = PackageInfo::deserialize(deserializer)?;

        Ok(info)
    }
}
