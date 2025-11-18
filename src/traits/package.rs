
pub trait Package {
// These are all async due to possible network requests/file IO
    
    // All of the published versions.
    async fn get_available_versions(&self) -> Result<Vec<Version>, NewDocsError>;

    // All of the available flags.
    async fn flags(&self) -> Result<Option<Vec<String>>, NewDocsError>;

    // A provided description as to the purpose of this package
    async fn description(&self) -> Result<Option<String>, NewDocsError>;

    // Any other packages that this package relies on (none is an empty array)
    async fn dependencies(&self) -> Result<Vec<Box<dyn Package>>, NewDocsError>;

    // Any other packages that rely on this package (none is an empty array)
    async fn dependents(&self) -> Result<Vec<Box<dyn Package>>, NewDocsError>;

    async fn retrieve(
        &self,
        version: Version,
        flags: Option<Vec<String>>,
    ) -> Result<String, NewDocsError>;
};
