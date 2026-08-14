//! `ExampleResolver`, a test resolver backed by an in-memory VFS.
use serde::{Deserialize, Serialize};
use vfs::{MemoryFS, VfsPath, error::VfsErrorKind};

use nudox_ir::prelude::*;

pub struct ExampleResolver {
    fs: VfsPath,
}

impl ExampleResolver {
    pub fn from_package(package: IrPackage<usize>) -> Self {
        let fs = VfsPath::new(MemoryFS::new());

        let dir = match package.info().id().view() {
            PackageIdView::Path(path) => fs.join(format!("{}", path.display())).unwrap(),
        };

        dir.create_dir()
            .expect("failed to create directory for package");

        let file = dir.join("info").unwrap().create_file().unwrap();

        let serializer = &mut serde_json::Serializer::new(file);

        package
            .info()
            .serialize(serializer)
            .expect("serializing PackageInfo failed");

        for (id, entry) in package.iter() {
            let id = match id {
                Some(id) => format!("{id}"),
                None => String::from("root"),
            };

            let file = dir.join(id).unwrap().create_file().unwrap();

            let serializer = &mut serde_json::Serializer::new(file);

            entry
                .serialize(serializer)
                .expect("serializing Entry failed");
        }

        ExampleResolver { fs }
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

    async fn load_unique_id(&mut self, id: &UniqueId<Self::EntryId>) -> Result<Entry, Self::Error> {
        let package = match id.package().view() {
            PackageIdView::Path(path) => format!("{}", path.display()),
        };

        let entry = match id.entry() {
            Some(entry) => format!("{entry}"),
            None => String::from("root"),
        };

        let file = self.fs.join(package)?.join(entry)?.open_file();

        let file = match file {
            Ok(file) => file,
            Err(err) if matches!(err.kind(), VfsErrorKind::FileNotFound) => {
                return Err(ResolverError::MissingEntry(UniqueId::clone(id)));
            }
            Err(err) => return Err(ResolverError::IO(err)),
        };

        let deserializer = &mut serde_json::Deserializer::from_reader(file);

        let entry = Entry::deserialize(deserializer)?;

        Ok(entry)
    }

    async fn load_package_info(
        &mut self,
        id: PackageId,
    ) -> Result<PackageInfo<Self::EntryId>, Self::Error> {
        let package = match id.view() {
            PackageIdView::Path(path) => format!("{}", path.display()),
        };

        let file = self.fs.join(package)?.join("info")?.open_file();

        let file = match file {
            Ok(file) => file,
            Err(err) if matches!(err.kind(), VfsErrorKind::FileNotFound) => {
                return Err(ResolverError::MissingPackage(id));
            }
            Err(err) => return Err(ResolverError::IO(err)),
        };

        let deserializer = &mut serde_json::Deserializer::from_reader(file);

        let info = PackageInfo::deserialize(deserializer)?;

        Ok(info)
    }
}
