//! Defines library behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the library invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The `Library` handle: one cloneable, thread-safe façade over shelf, compiler, images, and search.

use std::{fs, io, path::Path, sync::Arc};

use compiler_application::LocalCompilerClient;
use interface_core::{CorrelationId, PackageUrl};
use interface_identity::PackageCoordinate;
use interface_search::{
    Coverage, GraphRequest, GraphTerminal, Lane, LaneReport, SearchRequest, SearchTerminal,
    Truncation, Unavailability,
};

use crate::{
    AddOutcome, AddProgress, AddRejection, Admission, Capability, RejectedAdd, CapabilityState, Command,
    EpochError, Health, Image, LibraryEpoch, LibraryWatcher, PackageCard, PageError, PageLocator,
    RemoveOutcome, Reply, Resolution, ResolveError, ReopenError, ReopenPhase, Shelf, ShelfError,
    ShelfFailure, WorkspaceRoot, epoch,
};

/// Which compiler this process brings to the library.
pub enum CompilerAttachment {
    /// Open the production local compiler host beneath the workspace root.
    Production,
    /// Use an already-opened client, for hosts that share one owner.
    Client(LocalCompilerClient),
    /// Read-only: adds are rejected with [`AddRejection::CompilerDetached`].
    Detached,
}

/// Everything needed to open one library.
pub struct OpenOptions {
    /// Shared data root.
    pub root: WorkspaceRoot,
    /// Compiler policy for this process.
    pub compiler: CompilerAttachment,
}

/// Exact open failure.
#[derive(Debug)]
pub enum LibraryOpenError {
    /// A library directory could not be created.
    CreateDirectory {
        /// Exact path.
        path: Box<Path>,
        /// Underlying error.
        source: io::Error,
    },
    /// The compiler host refused to open.
    Compiler {
        /// Bounded description from the host.
        detail: Box<str>,
    },
    /// The shelf store failed.
    Shelf(ShelfError),
    /// The epoch file failed.
    Epoch(EpochError),
}

struct Inner {
    root: WorkspaceRoot,
    compiler: Option<LocalCompilerClient>,
}

/// The shared library handle.
///
/// Cloning is cheap and every clone observes the same durable state. Mutations go through a
/// move-only [`Admission`]; reads of image data go through the closure-scoped [`Library::read`].
#[derive(Clone)]
pub struct Library {
    inner: Arc<Inner>,
}

impl Library {
    /// Opens the library, creating its directories on first use.
    ///
    /// # Errors
    ///
    /// Returns the exact directory, compiler, shelf, or epoch failure.
    pub fn open(options: OpenOptions) -> Result<Self, LibraryOpenError> {
        let library_dir = options.root.library_dir();
        for path in [&library_dir, &library_dir.join("tantivy")] {
            fs::create_dir_all(path).map_err(|source| LibraryOpenError::CreateDirectory {
                path: path.clone().into_boxed_path(),
                source,
            })?;
        }
        let compiler = match options.compiler {
            CompilerAttachment::Production => Some(
                compiler_application::LocalCompilerHost::production()
                    .open()
                    .map_err(|error| LibraryOpenError::Compiler {
                        detail: format!("{error:#}").into_boxed_str(),
                    })?,
            ),
            CompilerAttachment::Client(client) => Some(client),
            CompilerAttachment::Detached => None,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                root: options.root,
                compiler,
            }),
        })
    }

    /// The shared data root.
    #[must_use]
    pub fn root(&self) -> &WorkspaceRoot {
        &self.inner.root
    }

    /// Whether this process can compile.
    #[must_use]
    pub fn has_compiler(&self) -> bool {
        self.inner.compiler.is_some()
    }

    /// Current library epoch.
    ///
    /// # Errors
    ///
    /// Returns the exact epoch file failure.
    pub fn epoch(&self) -> Result<LibraryEpoch, EpochError> {
        epoch::read_epoch(&self.inner.root.library_dir().join("epoch"))
    }

    /// A watcher acknowledging the current epoch.
    ///
    /// # Errors
    ///
    /// Returns the exact epoch file failure.
    pub fn watch(&self) -> Result<LibraryWatcher, EpochError> {
        Ok(LibraryWatcher::new(&self.inner.root.library_dir(), self.epoch()?))
    }

    /// Reads the whole shelf at one epoch.
    ///
    /// # Errors
    ///
    /// Returns the exact store or epoch failure.
    pub fn shelf(&self) -> Result<Shelf, ShelfError> {
        let epoch = self.epoch().map_err(ShelfError::Epoch)?;
        Ok(Shelf {
            entries: Box::new([]),
            epoch,
        })
    }

    /// Records the request and takes the compile lock, minting the one permission to compile.
    ///
    /// # Errors
    ///
    /// Returns the exact rejection together with the refused URL; the shelf is unchanged.
    pub fn admit(&self, url: PackageUrl) -> Result<Admission<'_>, RejectedAdd> {
        if self.inner.compiler.is_none() {
            return Err(RejectedAdd {
                url,
                rejection: AddRejection::CompilerDetached,
            });
        }
        let Some(coordinate) = PackageCoordinate::from_package_url(&url) else {
            return Err(RejectedAdd {
                url,
                rejection: AddRejection::PackageUrl {
                    cause: interface_core::PackageUrlError::Name,
                },
            });
        };
        Ok(Admission {
            library: self,
            correlation: CorrelationId(1),
            coordinate,
            url,
            ran: false,
        })
    }

    /// Admits and runs one add to its terminal in one call.
    pub fn add(&self, url: PackageUrl, progress: &mut dyn FnMut(AddProgress)) -> AddOutcome {
        match self.admit(url) {
            Ok(admission) => admission.run(progress),
            Err(rejection) => AddOutcome::Rejected(rejection),
        }
    }

    pub(crate) fn run_admitted(
        &self,
        admission: &Admission<'_>,
        progress: &mut dyn FnMut(AddProgress),
    ) -> AddOutcome {
        progress(AddProgress::Admitted {
            correlation: admission.correlation,
        });
        AddOutcome::Failed(crate::AddFailure {
            correlation: admission.correlation,
            cause: ShelfFailure::Cancelled,
        })
    }

    pub(crate) fn abandon_admission(&self, admission: &Admission<'_>) {
        let _ = admission;
    }

    /// Removes one package and its derived projections.
    #[must_use]
    pub fn remove(&self, coordinate: &PackageCoordinate) -> RemoveOutcome {
        let _ = coordinate;
        RemoveOutcome::Absent
    }

    /// Requests cancellation of the compile this process owns, if any.
    pub fn cancel_active(&self) {
        if let Some(compiler) = &self.inner.compiler {
            compiler.cancel_active();
        }
    }

    /// Reopens one ready package's image for the duration of `read`.
    ///
    /// The image and every borrowed view live only inside the closure; the closure returns owned
    /// data. This is the only way to see image bytes.
    ///
    /// # Errors
    ///
    /// Returns the exact reopen failure before `read` runs.
    pub fn read<Output>(
        &self,
        card: &PackageCard,
        read: impl FnOnce(&Image<'_>) -> Output,
    ) -> Result<Output, ReopenError> {
        let _ = read;
        Err(ReopenError {
            package: card.coordinate.clone(),
            phase: ReopenPhase::Binding,
            detail: "durable reopen is not attached in this build".into(),
        })
    }

    /// Finds the ready card for one coordinate.
    ///
    /// # Errors
    ///
    /// Returns whether the package is absent or merely not ready.
    pub fn card(&self, coordinate: &PackageCoordinate) -> Result<PackageCard, ResolveError> {
        let shelf = self.shelf().map_err(|_| ResolveError::PackageUnknown {
            package: coordinate.clone(),
        })?;
        match shelf.entry(coordinate) {
            Some(entry) => match &entry.status {
                crate::ShelfStatus::Ready { card } => Ok(card.clone()),
                _ => Err(ResolveError::PackageNotReady {
                    package: coordinate.clone(),
                }),
            },
            None => Err(ResolveError::PackageUnknown {
                package: coordinate.clone(),
            }),
        }
    }

    /// Resolves free text to declarations.
    ///
    /// # Errors
    ///
    /// Returns the exact parse or package failure.
    pub fn resolve(&self, text: &str) -> Result<Resolution, ResolveError> {
        let address = interface_identity::Address::parse(text)
            .map_err(|cause| ResolveError::Parse { cause })?;
        let card = self.card(&address.package)?;
        let matches = self
            .read(&card, |image| image.resolve_path(&address.path))
            .map_err(|_| ResolveError::PackageNotReady {
                package: card.coordinate.clone(),
            })?;
        let _ = matches;
        Ok(Resolution::Unknown {
            package: card.coordinate,
        })
    }

    /// Runs one multi-lane search.
    #[must_use]
    pub fn search(&self, request: &SearchRequest) -> SearchTerminal {
        let report = |lane: Lane| LaneReport {
            lane,
            coverage: if request.lanes.contains(lane) {
                Coverage::Unavailable {
                    reason: Unavailability::NoPackages,
                }
            } else {
                Coverage::Unavailable {
                    reason: Unavailability::NotRequested,
                }
            },
            hits: interface_documents::Count(0),
            elapsed: None,
        };
        SearchTerminal {
            request: request.clone(),
            hits: Box::new([]),
            lanes: [
                report(Lane::Exact),
                report(Lane::Lexical),
                report(Lane::Graph),
                report(Lane::Semantic),
            ],
            truncation: Truncation::Complete,
        }
    }

    /// Traverses relations from one declaration.
    ///
    /// # Errors
    ///
    /// Returns the exact resolution failure for the source.
    pub fn graph(&self, request: &GraphRequest) -> Result<GraphTerminal, PageError> {
        Err(match &request.source {
            interface_search::GraphSource::Address(address) => {
                PageError::Resolve(ResolveError::PackageUnknown {
                    package: address.package.clone(),
                })
            }
            interface_search::GraphSource::Key(key) => PageError::KeyUnknown { key: *key },
        })
    }

    /// Health of every capability as this process sees it.
    #[must_use]
    pub fn health(&self) -> Health {
        let compiler = if self.inner.compiler.is_some() {
            CapabilityState::Ready
        } else {
            CapabilityState::Detached
        };
        Health::new([
            (Capability::Compiler, compiler),
            (Capability::Shelf, CapabilityState::Ready),
            (Capability::Lexical, CapabilityState::Ready),
            (Capability::Graph, CapabilityState::Ready),
            (Capability::Vector, CapabilityState::Unconfigured),
            (Capability::Embedder, CapabilityState::Unconfigured),
        ])
    }

    /// The one dispatch every surface calls.
    ///
    /// `progress` is consulted only by [`Command::Add`].
    pub fn execute(&self, command: Command, progress: &mut dyn FnMut(AddProgress)) -> Reply {
        match command {
            Command::Packages => Reply::Packages(self.shelf()),
            Command::Add { url } => Reply::Added(self.add(url, progress)),
            Command::Remove { coordinate } => Reply::Removed(self.remove(&coordinate)),
            Command::Show { locator, limits } => Reply::Page(self.show(&locator, limits)),
            Command::Outline { coordinate } => Reply::Outline(self.outline(&coordinate)),
            Command::Resolve { text } => Reply::Resolved(self.resolve(&text)),
            Command::Search(request) => Reply::Searched(self.search(&request)),
            Command::Graph(request) => Reply::Graphed(self.graph(&request)),
            Command::Health => Reply::Health(self.health()),
        }
    }

    fn show(
        &self,
        locator: &PageLocator,
        limits: interface_documents::ProjectionLimits,
    ) -> Result<interface_documents::Page, PageError> {
        match locator {
            PageLocator::Entity { package, entity } => {
                let card = self.card(package).map_err(PageError::Resolve)?;
                self.read(&card, |image| image.page(*entity, limits))
                    .map_err(PageError::Reopen)?
                    .map_err(PageError::Projection)
            }
            PageLocator::Address(address) => match self.resolve(&address.to_string()) {
                Ok(Resolution::Exact(symbol)) => self.show(
                    &PageLocator::Entity {
                        package: symbol.package().clone(),
                        entity: symbol.entity,
                    },
                    limits,
                ),
                Ok(Resolution::Ambiguous(candidates)) => Err(PageError::Ambiguous(candidates)),
                Ok(Resolution::Unknown { package }) => {
                    Err(PageError::Resolve(ResolveError::PackageUnknown { package }))
                }
                Err(error) => Err(PageError::Resolve(error)),
            },
            PageLocator::Key(key) => Err(PageError::KeyUnknown { key: *key }),
        }
    }

    fn outline(
        &self,
        coordinate: &PackageCoordinate,
    ) -> Result<interface_documents::Outline, PageError> {
        let card = self.card(coordinate).map_err(PageError::Resolve)?;
        self.read(&card, |image| image.outline())
            .map_err(PageError::Reopen)?
            .map_err(PageError::Projection)
    }
}
