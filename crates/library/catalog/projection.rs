//! Bounded catalog projections and query cursor admission.

use super::Library;
use crate::arrangement::ArrangementPage;
use crate::{
    Cursor, Document, DocumentQuery, Freshness, Frontier, GraphNeighborhoodQuery, GraphRelation,
    LibraryError, NameQuery, Outline, OutlineExtent, OutlineNode, OutlineQuery, PackageKey,
    OutlineCursorWitness, PageRequest, PageTerminal, ProjectionPage, Query, QueryLimit, QueryWindow,
    RankedSearchSnapshot, ReadManifest, Row, RowId, SymbolKey, ViewRecipeId, ViewRevision,
    ViewRoot, ViewSnapshot, ViewStateRoot, view_identity_bytes,
};
use crate::{ReferenceFact, ReferenceRecord};
use std::collections::{BTreeMap, BTreeSet};

/// Query family and page credit are part of the continuation contract.
#[derive(Clone, Copy)]
pub(crate) enum QueryPageKind {
    Names,
    Search,
}

pub(crate) fn query_page_preimage(kind: QueryPageKind, text: &str, limit: QueryLimit) -> Vec<u8> {
    let mut bytes = b"catalog-query-page-v2\0".to_vec();
    bytes.push(match kind {
        QueryPageKind::Names => 0,
        QueryPageKind::Search => 1,
    });
    bytes.extend_from_slice(&limit.get().to_be_bytes());
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

/// Canonical witness for a bounded catalog page's continuation contract.
///
/// Producers, certificate builders, and consumers use the same canonical
/// encoding. Names/search bind text, credit and optional read manifest. Windowed
/// graph pages bind their address/root and issued credit; legacy graph recipes
/// retain their exact historical address/root encoding.
/// Private fields keep the identity paired with its exact preimage.
#[derive(Debug)]
pub struct QueryPageRecipe {
    identity: ViewRecipeId,
    preimage: Box<[u8]>,
}

const PACKAGES_PAGE_RECIPE: &[u8] = b"packages-page";

impl QueryPageRecipe {
    /// Binds a legacy fixed package page to its selected source root.
    #[must_use]
    pub fn packages(root: ViewStateRoot) -> Self {
        let preimage = crate::canonical::view_identity_preimage(&[
            b"query",
            PACKAGES_PAGE_RECIPE,
            root.as_bytes(),
            &[0],
        ])
        .into_boxed_slice();
        Self {
            identity: ViewRecipeId::from_value(&preimage),
            preimage,
        }
    }
    /// Binds a names request to its selected source root.
    #[must_use]
    pub fn names(root: ViewStateRoot, query: &NameQuery) -> Self {
        Self::new(
            QueryPageKind::Names,
            query.text(),
            query.window(),
            root,
            query.read_manifest(),
        )
    }

    /// Binds a search request to its selected source root.
    #[must_use]
    pub fn search(root: ViewStateRoot, query: &Query) -> Self {
        Self::new(
            QueryPageKind::Search,
            query.text(),
            query.window(),
            root,
            query.read_manifest(),
        )
    }

    /// Borrows the original request while authenticating its exact predecessor recipe.
    #[must_use]
    pub fn names_predecessor(root: ViewStateRoot, query: &NameQuery) -> Option<Self> {
        let window = Self::predecessor_window(query.window(), query.cursor())?;
        Some(Self::new(
            QueryPageKind::Names,
            query.text(),
            window,
            root,
            query.read_manifest(),
        ))
    }

    /// Borrows the original request while authenticating its exact predecessor recipe.
    #[must_use]
    pub fn search_predecessor(root: ViewStateRoot, query: &Query) -> Option<Self> {
        let window = Self::predecessor_window(query.window(), query.cursor())?;
        Some(Self::new(
            QueryPageKind::Search,
            query.text(),
            window,
            root,
            query.read_manifest(),
        ))
    }

    /// Binds a current published-name stage independently of full lexical search.
    #[must_use]
    pub fn published_names(root: ViewStateRoot, request: &crate::PublishedNameSearchQuery) -> Self {
        let query = request.query();
        Self::published_names_at_window(root, request, query.window())
    }

    /// Authenticates this stage's exact previous delivery credit.
    #[must_use]
    pub fn published_names_predecessor(
        root: ViewStateRoot,
        request: &crate::PublishedNameSearchQuery,
    ) -> Option<Self> {
        let window = Self::predecessor_window(request.query().window(), request.query().cursor())?;
        Some(Self::published_names_at_window(root, request, window))
    }

    fn published_names_at_window(
        root: ViewStateRoot,
        request: &crate::PublishedNameSearchQuery,
        window: QueryWindow,
    ) -> Self {
        let query = request.query();
        let base = Self::new(
            QueryPageKind::Names,
            query.text(),
            window,
            root,
            query.read_manifest(),
        );
        let preimage = crate::canonical::view_identity_preimage(&[
            b"published-name-search-v1",
            base.canonical_preimage(),
        ])
        .into_boxed_slice();
        Self {
            identity: ViewRecipeId::from_value(&preimage),
            preimage,
        }
    }

    /// Authenticates legacy predecessors before issuing a new windowed graph recipe.
    #[must_use]
    pub fn graph_predecessor(
        root: ViewStateRoot,
        address: crate::SymbolAddress,
        page: PageRequest,
    ) -> Option<Self> {
        let window = Self::predecessor_window(
            page.window(),
            page.continuation().map(crate::PageContinuation::cursor),
        )?;
        Some(Self::graph_window(root, address, window))
    }

    fn predecessor_window(window: QueryWindow, cursor: Option<Cursor>) -> Option<QueryWindow> {
        cursor.map_or(Some(window), |cursor| {
            window.predecessor(cursor.query_position())
        })
    }

    /// Binds a plain graph page to its exact source root and original address.
    ///
    /// This retains the existing graph-page recipe. The exact page credit is
    /// carried by the continued command and checked against its predecessor.
    #[must_use]
    pub fn graph(root: ViewStateRoot, address: crate::SymbolAddress) -> Self {
        Self::graph_window(root, address, QueryWindow::fixed(QueryLimit::default()))
    }

    /// Windowed graph credit is bound natively, independently of delivery width.
    #[must_use]
    pub fn graph_window(
        root: ViewStateRoot,
        address: crate::SymbolAddress,
        window: QueryWindow,
    ) -> Self {
        let recipe = graph_page_recipe(address, window);
        let preimage =
            crate::canonical::view_identity_preimage(&[b"query", &recipe, root.as_bytes(), &[0]])
                .into_boxed_slice();
        Self {
            identity: ViewRecipeId::from_value(&preimage),
            preimage,
        }
    }

    /// Binds immediate membership to its exact selected owner, source and credit.
    #[must_use]
    pub fn members(root: ViewStateRoot, owner: crate::SymbolAddress, limit: QueryLimit) -> Self {
        let recipe = members_page_recipe(owner, limit);
        let preimage =
            crate::canonical::view_identity_preimage(&[b"query", &recipe, root.as_bytes(), &[0]])
                .into_boxed_slice();
        Self {
            identity: ViewRecipeId::from_value(&preimage),
            preimage,
        }
    }

    /// Binds one outline page's exact native order to its package, source,
    /// fixed credit and absolute start. The order digest is the final segment
    /// so portable clients can revalidate its canonical predecessor context.
    #[must_use]
    pub fn outline(root: ViewStateRoot, package: PackageKey, limit: QueryLimit, order: &crate::OutlinePageOrder) -> Self {
        Self::outline_window(root, package, QueryWindow::fixed(limit), order)
    }

    /// Binds a complete outline delivery to its unchanged issued ceiling,
    /// actual delivery bound, absolute order and exact immutable source.
    #[must_use]
    pub fn outline_window(root: ViewStateRoot, package: PackageKey, window: QueryWindow, order: &crate::OutlinePageOrder) -> Self {
        Self::outline_window_digest(root, package, window, order.start(), order.digest())
    }

    fn outline_window_digest(root: ViewStateRoot, package: PackageKey, window: QueryWindow, start: u64, digest: [u8; 32]) -> Self {
        let mut recipe = if window.is_windowed() {
            let mut bytes = b"outline-window-v1\0".to_vec();
            bytes.extend_from_slice(package.as_bytes());
            bytes.extend_from_slice(&window.issued().get().to_be_bytes());
            bytes.extend_from_slice(&window.delivery().get().to_be_bytes());
            bytes
        } else { outline_page_recipe(package, window.issued()) };
        recipe.extend_from_slice(&start.to_be_bytes());
        let preimage = crate::canonical::view_identity_preimage(&[
            b"query", &recipe, root.as_bytes(), &digest,
        ]).into_boxed_slice();
        Self { identity: ViewRecipeId::from_value(&preimage), preimage }
    }

    /// Rehashes a portable predecessor's complete canonical recipe. Its opaque
    /// order digest supplies no authority: the owner still reconstructs the
    /// exact prior ordered rows before admitting a successor.
    #[must_use]
    pub fn outline_predecessor(root: ViewStateRoot, package: PackageKey, limit: QueryLimit, cursor: Cursor, preimage: &[u8]) -> Option<Self> {
        Self::outline_window_predecessor(root, package, QueryWindow::fixed(limit), cursor, preimage)
    }

    /// Rehashes the exact prior outline span, retaining fixed legacy recipes
    /// until a native windowed successor is actually issued.
    #[must_use]
    pub fn outline_window_predecessor(root: ViewStateRoot, package: PackageKey, window: QueryWindow, cursor: Cursor, preimage: &[u8]) -> Option<Self> {
        let previous = window.predecessor(cursor.query_position())?;
        let start = cursor.query_offset().checked_sub(u64::from(previous.delivery().get()))?;
        let digest: [u8; 32] = preimage.get(preimage.len().checked_sub(32)?..)?.try_into().ok()?;
        let expected = Self::outline_window_digest(root, package, previous, start, digest);
        (expected.canonical_preimage() == preimage && expected.identity() == cursor.recipe()).then_some(expected)
    }

    /// Rehashes an outline predecessor's complete recipe without promoting
    /// its page-root commitment. Its order digest is only a witness: the
    /// owning [`Library`] must still reconstruct the ordered rows before use.
    #[must_use]
    pub fn outline_window_witness_predecessor(
        root: ViewStateRoot,
        package: PackageKey,
        window: QueryWindow,
        witness: crate::OutlineCursorWitness,
        preimage: &[u8],
    ) -> Option<Self> {
        let previous = window.predecessor(witness.query_position())?;
        let start = witness
            .query_position()
            .offset()
            .checked_sub(u64::from(previous.delivery().get()))?;
        let digest: [u8; 32] = preimage.get(preimage.len().checked_sub(32)?..)?.try_into().ok()?;
        let expected = Self::outline_window_digest(root, package, previous, start, digest);
        (expected.canonical_preimage() == preimage && expected.identity() == witness.recipe())
            .then_some(expected)
    }

    fn new(
        kind: QueryPageKind,
        text: &str,
        window: QueryWindow,
        root: ViewStateRoot,
        manifest: Option<&ReadManifest>,
    ) -> Self {
        let recipe = if window.is_windowed() {
            let mut bytes = b"catalog-query-window-v1\0".to_vec();
            bytes.push(match kind {
                QueryPageKind::Names => 0,
                QueryPageKind::Search => 1,
            });
            bytes.extend_from_slice(&window.issued().get().to_be_bytes());
            bytes.extend_from_slice(text.as_bytes());
            bytes
        } else {
            query_page_preimage(kind, text, window.issued())
        };
        let manifest = manifest_preimage(manifest);
        let preimage = crate::canonical::view_identity_preimage(&[
            b"query",
            &recipe,
            root.as_bytes(),
            &manifest,
        ])
        .into_boxed_slice();
        Self {
            identity: ViewRecipeId::from_value(&preimage),
            preimage,
        }
    }

    /// The identity admitted by this canonical witness.
    #[must_use]
    pub const fn identity(&self) -> ViewRecipeId {
        self.identity
    }

    /// Exact recipe bytes carried by a wire identity certificate.
    #[must_use]
    pub fn canonical_preimage(&self) -> &[u8] {
        &self.preimage
    }
}

pub(crate) fn outline_page_recipe(package: PackageKey, limit: QueryLimit) -> Vec<u8> {
    let mut bytes = b"outline-page-v4\0".to_vec();
    bytes.extend_from_slice(package.as_bytes());
    bytes.extend_from_slice(&limit.get().to_be_bytes());
    bytes
}

fn members_page_recipe(owner: crate::SymbolAddress, limit: QueryLimit) -> Vec<u8> {
    let mut bytes = b"members-page-v1\0".to_vec();
    bytes.push(u8::from(owner.is_selected()));
    bytes.extend_from_slice(&owner.claimed_bytes());
    bytes.extend_from_slice(&limit.get().to_be_bytes());
    bytes
}

fn graph_page_recipe(address: crate::SymbolAddress, window: QueryWindow) -> Vec<u8> {
    let mut bytes = if window.is_windowed() {
        b"graph-page-window-v1\0".to_vec()
    } else {
        b"graph-page".to_vec()
    };
    if window.is_windowed() {
        bytes.extend_from_slice(&window.issued().get().to_be_bytes());
    }
    bytes.extend_from_slice(&address.claimed_bytes());
    bytes
}

fn manifest_preimage(manifest: Option<&ReadManifest>) -> Vec<u8> {
    let mut bytes = vec![u8::from(manifest.is_some())];
    if let Some(manifest) = manifest {
        bytes.extend_from_slice(&manifest.canonical_bytes());
    }
    bytes
}

pub(crate) fn projection_recipe(
    recipe: &[u8],
    root: ViewStateRoot,
    manifest: Option<&ReadManifest>,
) -> ViewRecipeId {
    let bytes = manifest_preimage(manifest);
    projection_recipe_with_manifest(recipe, root, &bytes)
}

fn projection_recipe_with_manifest(
    recipe: &[u8],
    root: ViewStateRoot,
    manifest: &[u8],
) -> ViewRecipeId {
    view_identity_bytes(&[b"query", recipe, root.as_bytes(), manifest])
}

impl Library {
    /// Reads one root/query-bound package page.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale revision,
    /// [`LibraryError::CursorMismatch`] for a foreign continuation, or
    /// [`LibraryError::View`] when the bounded snapshot cannot be built.
    pub fn packages_page(&self, page: PageRequest) -> Result<ProjectionPage, LibraryError> {
        self.check_basis(page.basis())?;
        if page.window().is_windowed()
            || page
                .continuation()
                .is_some_and(|cursor| cursor.cursor().query_position().predecessor().is_some())
        {
            return Err(LibraryError::InvalidQuery(
                "this projection requires a legacy fixed page and position".to_owned(),
            ));
        }
        let recipe_bytes = PACKAGES_PAGE_RECIPE;
        let recipe = self.query_recipe(recipe_bytes, None);
        let limit = usize::from(page.limit().get());
        let cursor = page.continuation().map(crate::PageContinuation::cursor);
        let start = self.check_query_cursor(cursor, recipe, limit, |offset, count| {
            self.arrangement.packages_page_from(offset, count)
        })?;
        let result = self.arrangement.packages_page_from(start, limit);
        self.projection_page(recipe_bytes, &result, start, limit)
    }

    /// Reconstructs the exact prior outline page named by a digest-only
    /// witness. The supplied root commitment remains untrusted until this
    /// method has fetched the predecessor rows and rebuilt their canonical
    /// `ViewRoot` at the current durable basis.
    ///
    /// # Errors
    /// Refuses a stale basis, changed recipe/window/order, foreign frontier,
    /// missing predecessor rows, or a root/version commitment mismatch.
    pub fn rehydrate_outline_cursor(
        &self,
        package: PackageKey,
        basis: ViewRevision,
        window: QueryWindow,
        witness: OutlineCursorWitness,
        recipe_preimage: &[u8],
    ) -> Result<Cursor, LibraryError> {
        self.check_basis(basis)?;
        if !self.arrangement.has_package(package)
            || witness.branch() != self.cursor.branch()
            || witness.log() != self.cursor.log()
            || witness.schema() != self.cursor.schema()
            || witness.sequence() > self.cursor.sequence()
        {
            return Err(LibraryError::CursorMismatch);
        }
        let previous_window = window
            .predecessor(witness.query_position())
            .ok_or(LibraryError::CursorMismatch)?;
        let page_size = usize::from(previous_window.delivery().get());
        let offset = usize::try_from(witness.query_position().offset())
            .map_err(|_| LibraryError::CursorMismatch)?;
        let start = offset.checked_sub(page_size).ok_or(LibraryError::CursorMismatch)?;
        let predecessor = self.arrangement.outline_page(
            package,
            start,
            page_size,
            |id| self.view.row_ref(id),
            &self.work,
        );
        if predecessor.ids.len() != page_size {
            return Err(LibraryError::CursorMismatch);
        }
        let symbols = predecessor
            .ids
            .iter()
            .filter_map(|id| match id {
                RowId::Symbol(symbol) => Some(*symbol),
                _ => None,
            })
            .collect::<Vec<_>>();
        let order = crate::OutlinePageOrder::new(start as u64, symbols)
            .map_err(LibraryError::InvalidQuery)?;
        let recipe = QueryPageRecipe::outline_window(
            self.revision_root(),
            package,
            previous_window,
            &order,
        );
        if recipe.identity() != witness.recipe()
            || recipe.canonical_preimage() != recipe_preimage
        {
            return Err(LibraryError::CursorMismatch);
        }

        let single_package = self.arrangement.package_count() == 1;
        let rows = predecessor
            .ids
            .iter()
            .filter_map(|&id| {
                self.view.row(id).map(|mut row| {
                    if single_package && row.package.is_none() {
                        row.package = Some(package);
                    }
                    row
                })
            })
            .collect::<Vec<_>>();
        if rows.len() != page_size {
            return Err(LibraryError::CursorMismatch);
        }
        let snapshot = self.snapshot_with_recipe(recipe.identity(), rows, None, None)?;
        let root = &snapshot.root;
        if root.root().as_bytes() != &witness.root_commitment()
            || root.version() != witness.version()
            || root.recipe() != witness.recipe()
            || root.frontier().branch != witness.branch()
            || root.frontier().log != witness.log()
            || root.frontier().schema != witness.schema()
        {
            return Err(LibraryError::CursorMismatch);
        }
        Ok(Cursor::for_view(
            root.recipe(),
            root.version(),
            Frontier::new(
                witness.branch(),
                witness.log(),
                witness.schema(),
                root.root(),
                witness.sequence(),
            ),
        )
        .with_query_position(witness.query_position()))
    }

    /// Reads package outline membership as one bounded flat row page.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale revision,
    /// [`LibraryError::NotFound`] for an unknown package,
    /// [`LibraryError::CursorMismatch`] for a foreign continuation, or
    /// [`LibraryError::View`] when the bounded snapshot cannot be built.
    pub fn outline_page(
        &self,
        package: PackageKey,
        page: PageRequest,
    ) -> Result<ProjectionPage, LibraryError> {
        if page.outline_witness().is_some() {
            return Err(LibraryError::CursorMismatch);
        }
        self.check_basis(page.basis())?;
        if !self.arrangement.has_package(package) {
            return Err(LibraryError::NotFound);
        }
        let window = page.window();
        let limit = usize::from(window.issued().get());
        let delivery = usize::from(window.delivery().get());
        let fetch = |offset, count| {
            self.arrangement.outline_page(package, offset, count, |id| self.view.row_ref(id), &self.work)
        };
        // The native arrangement alone can prove the historical singleton
        // package fallback. Materialize that inferred membership in the page's
        // certified rows; consumers must never infer it from a bounded page.
        let single_package = self.arrangement.package_count() == 1;
        let page_row = |id| self.view.row(id).map(|mut row| {
            if single_package && row.package.is_none() {
                row.package = Some(package);
            }
            row
        });
        let symbols = |ids: &[RowId]| ids.iter().filter_map(|id| match id {
            RowId::Symbol(symbol) => Some(*symbol), _ => None,
        }).collect::<Vec<_>>();
        let start = if let Some(cursor) = page.continuation().map(crate::PageContinuation::cursor) {
            let offset = usize::try_from(cursor.query_offset()).map_err(|_| LibraryError::CursorMismatch)?;
            let previous_window = window.predecessor(cursor.query_position()).ok_or(LibraryError::CursorMismatch)?;
            let previous_count = usize::from(previous_window.delivery().get());
            let previous = offset.checked_sub(previous_count).ok_or(LibraryError::CursorMismatch)?;
            let predecessor = fetch(previous, previous_count);
            let order = crate::OutlinePageOrder::new(previous as u64, symbols(&predecessor.ids))
                .map_err(LibraryError::InvalidQuery)?;
            let recipe = QueryPageRecipe::outline_window(self.revision_root(), package, previous_window, &order);
            self.check_query_cursor_with_rows(Some(cursor), recipe.identity(), limit, |at, count| {
                if at == previous && count == previous_count {
                    ArrangementPage { ids: predecessor.ids.clone(), has_more: predecessor.has_more }
                } else { fetch(at, count) }
            }, |id, _| page_row(id))?
        } else { 0 };
        let result = fetch(start, delivery);
        let order = crate::OutlinePageOrder::new(start as u64, symbols(&result.ids))
            .map_err(LibraryError::InvalidQuery)?;
        let recipe = QueryPageRecipe::outline_window(self.revision_root(), package, window, &order);
        let rows = result.ids.iter().filter_map(|&id| page_row(id)).collect();
        let snapshot = self.snapshot_with_window(
            recipe.identity(), rows, result.has_more.then_some(self.cursor), start, window,
        )?;
        order.validate(&snapshot, package, page.limit()).map_err(LibraryError::InvalidQuery)?;
        let terminal = snapshot.next.map_or(PageTerminal::Complete, |cursor| {
            PageTerminal::More(crate::PageContinuation::from_cursor(cursor))
        });
        Ok(ProjectionPage { snapshot, terminal, outline_order: Some(order) })
    }

    /// Pages one retained owner and its immediate members from the existing
    /// parent arrangement. Unrelated package rows and semantic images are never
    /// visited. The owner prefix proves membership even for an empty member set.
    ///
    /// # Errors
    /// Refuses a stale source, missing owner, changed credit or foreign cursor.
    pub fn members_page(
        &self,
        owner: DocumentQuery,
        page: PageRequest,
    ) -> Result<ProjectionPage, LibraryError> {
        self.check_basis(owner.basis())?;
        self.check_basis(page.basis())?;
        if owner.basis() != page.basis()
            || owner
                .source_basis()
                .is_some_and(|source| source != self.revision_basis())
            || page.window().is_windowed()
            || page
                .continuation()
                .is_some_and(|cursor| cursor.cursor().query_position().predecessor().is_some())
        {
            return Err(LibraryError::InvalidQuery(
                "member page changed its owner/source/fixed credit".to_owned(),
            ));
        }
        let symbol = owner
            .resolve_symbol(&self.view)
            .ok_or(LibraryError::NotFound)?;
        let root = self
            .view
            .row_ref(RowId::Symbol(symbol))
            .ok_or(LibraryError::NotFound)?;
        let recipe_bytes = members_page_recipe(owner.symbol(), page.limit());
        let recipe = self.query_recipe(&recipe_bytes, None);
        let limit = usize::from(page.limit().get());
        let cursor = page.continuation().map(crate::PageContinuation::cursor);
        let start = self.check_query_cursor(cursor, recipe, limit, |offset, count| {
            self.member_page_ids(root, symbol, offset, count)
        })?;
        let result = self.member_page_ids(root, symbol, start, limit);
        self.projection_page(&recipe_bytes, &result, start, limit)
    }

    fn member_page_ids(
        &self,
        root: &Row,
        symbol: SymbolKey,
        start: usize,
        limit: usize,
    ) -> ArrangementPage {
        let mut ids = Vec::with_capacity(limit.saturating_add(1));
        if start == 0 {
            ids.push(RowId::Symbol(symbol));
        }
        let mut has_more = false;
        if let Some(package) = root.package {
            let children = self.arrangement.children_page_from(
                package,
                Some(symbol),
                start.saturating_sub(1),
                limit.saturating_add(1).saturating_sub(ids.len()),
            );
            has_more = children.has_more;
            ids.extend(children.ids);
        }
        has_more |= ids.len() > limit;
        ids.truncate(limit);
        self.work.record_seek();
        self.work.record_output(ids.len());
        ArrangementPage { ids, has_more }
    }

    /// Looks up one document under an exact source basis.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] when the query is stale and
    /// [`LibraryError::NotFound`] when no document is registered for the
    /// requested symbol.
    pub fn document(&self, query: DocumentQuery) -> Result<Document, LibraryError> {
        let basis = self.check_basis(query.basis)?;
        if let Some(source) = query.source_basis()
            && source != self.revision_basis()
        {
            return Err(LibraryError::WrongBasis {
                expected: self.revision_root(),
                observed: source.root.into(),
            });
        }
        let symbol = query
            .resolve_symbol(&self.view)
            .ok_or(LibraryError::NotFound)?;
        self.work.record_seek();
        let row = self
            .arrangement
            .document(symbol)
            .then(|| self.view.row(RowId::Symbol(symbol)))
            .flatten()
            .ok_or(LibraryError::NotFound)?;
        let mut document = Document::new(symbol, basis, row.document.clone())
            .with_source_basis(self.revision_basis())
            .with_location(row.source.clone())
            .with_excerpt(row.excerpt.clone());
        document.signature.clone_from(&row.signature);
        Ok(document)
    }

    /// Looks up names with a bounded stable-row result.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale query and
    /// [`LibraryError::View`] when the bounded snapshot cannot be constructed.
    pub fn names(&self, query: &NameQuery) -> Result<ViewSnapshot, LibraryError> {
        let recipe = QueryPageRecipe::names(self.revision_root(), query).identity();
        let predecessor = QueryPageRecipe::names_predecessor(self.revision_root(), query)
            .ok_or(LibraryError::CursorMismatch)?
            .identity();
        self.names_at_recipe(query, recipe, predecessor)
    }

    /// Reads current owner-bound names without opening compiler images.
    /// This cursor belongs exclusively to the published-name search stage.
    ///
    /// # Errors
    /// Refuses stale source roots and changed stage/query/credit continuations.
    pub fn search_published_names(
        &self,
        request: &crate::PublishedNameSearchQuery,
    ) -> Result<ViewSnapshot, LibraryError> {
        let query = request.query();
        let names = NameQuery {
            text: query.text.clone(),
            basis: query.basis,
            window: query.window,
            cursor: query.cursor,
            read_manifest: query.read_manifest.clone(),
        };
        let recipe = QueryPageRecipe::published_names(self.revision_root(), request).identity();
        let predecessor =
            QueryPageRecipe::published_names_predecessor(self.revision_root(), request)
                .ok_or(LibraryError::CursorMismatch)?
                .identity();
        self.names_at_recipe(&names, recipe, predecessor)
    }

    fn names_at_recipe(
        &self,
        query: &NameQuery,
        recipe: ViewRecipeId,
        previous_recipe: ViewRecipeId,
    ) -> Result<ViewSnapshot, LibraryError> {
        self.check_basis(query.basis)?;
        self.work.record_seek();
        let window = query.window();
        let limit = usize::from(window.issued().get());
        let delivery = usize::from(window.delivery().get());
        let fetch = |offset, count| {
            self.arrangement.names_page(
                query.selection(),
                offset,
                count,
                |id| self.view.row_ref(id),
                |id, address| {
                    self.view
                        .row_ref(id)
                        .is_some_and(|row| self.row_matches_address(row, address))
                },
                |id, text| self.row_matches_relative_name(id, text),
                &self.work,
            )
        };
        // Canonical snapshots sort identities; absolute ordinal scores preserve
        // the selected names order in presentation across every page size.
        let ranked_row = |id, rank| {
            let mut row = self.view.row(id)?;
            row.score = Some(u32::MAX.checked_sub(u32::try_from(rank).ok()?)?);
            Some(row)
        };
        let start = self.check_query_cursor_with_rows(
            query.cursor,
            previous_recipe,
            limit,
            fetch,
            ranked_row,
        )?;
        let page = fetch(start, delivery);
        if query.cursor.is_some() && page.ids.is_empty() {
            return Err(LibraryError::CursorMismatch);
        }
        let rows = page
            .ids
            .iter()
            .enumerate()
            .filter_map(|(at, &id)| ranked_row(id, start + at))
            .collect::<Vec<_>>();
        self.work.record_output(rows.len());
        self.snapshot_with_window(
            recipe,
            rows,
            if page.has_more {
                Some(self.cursor)
            } else {
                None
            },
            start,
            window,
        )
    }

    /// Resolves relative member names only through admitted immediate parents
    /// in this exact view and package. The optional boolean preserves exact
    /// versus substring leaf ranking without rewriting any native coordinate.
    fn row_matches_relative_name(&self, id: RowId, text: &str) -> Option<bool> {
        let (mut owners, leaf) = crate::command::relative_name_tail(text)?;
        let mut row = self.view.row_ref(id)?;
        let name = row.label.rsplit("::").next()?;
        let normalized_name = name.to_lowercase();
        let normalized_leaf = leaf.to_lowercase();
        if !normalized_name.contains(&normalized_leaf) {
            return None;
        }
        let exact = normalized_name == normalized_leaf;
        // An absent owner is not a shared scope. Legacy unscoped labels
        // retain direct-label matching, but cannot lend relative ancestry.
        let package = row.package?;
        let mut seen = BTreeSet::from([row.id]);
        loop {
            let (remaining, owner) = crate::command::relative_name_tail(owners)
                .map_or((None, owners), |(remaining, owner)| {
                    (Some(remaining), owner)
                });
            // A trailing/missing segment is not an owner name. Do not repair
            // malformed syntax by skipping empty requested ancestors.
            if owner.is_empty() || owner.ends_with('.') || owner.ends_with("::") {
                return None;
            }
            row = self.view.row_ref(RowId::Symbol(row.parent?))?;
            if row.package != Some(package)
                || !seen.insert(row.id)
                || row.label.rsplit("::").next()?.to_lowercase() != owner.to_lowercase()
            {
                return None;
            }
            let Some(remaining) = remaining else {
                return Some(exact);
            };
            owners = remaining;
        }
    }

    /// Matches only producer-admitted address evidence. A readable source
    /// address selects the same compiler-owned row as its semantic address;
    /// the package prefix and captured site are read from this exact view.
    fn row_matches_address(
        &self,
        row: &Row,
        address: crate::command::AddressSelection<'_>,
    ) -> bool {
        if row.label == address.text {
            return true;
        }
        let Some(site) = address.source else {
            return false;
        };
        if !row
            .label
            .strip_suffix(site.name)
            .is_some_and(|prefix| prefix.ends_with("::"))
        {
            return false;
        }
        let Some(location) = row.source.captured() else {
            return false;
        };
        let Some(package) = row.package else {
            return false;
        };
        let Some(package_row) = self.view.row_ref(RowId::Package(package)) else {
            return false;
        };
        site.package == package_row.label
            && site.path == location.path()
            && site.line == location.start_line()
    }

    /// Looks up one package outline under an exact source basis.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale query and
    /// [`LibraryError::NotFound`] when no outline is registered for the
    /// requested package.
    pub fn outline(&self, query: OutlineQuery) -> Result<Outline, LibraryError> {
        let basis = self.check_basis(query.basis)?;
        if let Some(source) = query.source_basis()
            && source != self.revision_basis()
        {
            return Err(LibraryError::WrongBasis {
                expected: self.revision_root(),
                observed: source.root.into(),
            });
        }
        self.work.record_seek();
        if !self.arrangement.has_package(query.package) {
            return Err(LibraryError::NotFound);
        }
        let page = self.arrangement.outline_page(
            query.package,
            0,
            usize::from(QueryLimit::MAX),
            |id| self.view.row_ref(id),
            &self.work,
        );
        let mut truncated = page.has_more;
        let symbols = page
            .ids
            .iter()
            .filter_map(|id| match id {
                RowId::Symbol(symbol) => Some(*symbol),
                _ => None,
            })
            .collect::<Vec<_>>();
        if symbols.is_empty() {
            return Err(LibraryError::NotFound);
        }
        let selected = symbols.iter().copied().collect::<BTreeSet<_>>();
        let mut children = BTreeMap::<SymbolKey, Vec<SymbolKey>>::new();
        let mut roots = Vec::new();
        for symbol in &symbols {
            let parent = self
                .view
                .row_ref(RowId::Symbol(*symbol))
                .and_then(|row| row.parent);
            if let Some(parent) = parent.filter(|parent| selected.contains(parent)) {
                children.entry(parent).or_default().push(*symbol);
            } else {
                // An out-of-page parent stays a retained row fact; this bounded
                // forest promotes its child for navigation, without inventing an edge.
                roots.push(*symbol);
            }
        }
        let mut visited = BTreeSet::new();
        let mut nodes = Vec::new();
        for symbol in roots.into_iter().chain(symbols.iter().copied()) {
            if !visited.contains(&symbol) {
                nodes.push(Self::outline_selected_node(
                    symbol,
                    &children,
                    &mut visited,
                    0,
                    &mut truncated,
                ));
            }
        }
        let root = nodes.remove(0);
        Ok(Outline::new(query.package, basis, root)
            .with_additional_roots(nodes)
            .with_extent(if truncated {
                OutlineExtent::Truncated
            } else {
                OutlineExtent::Complete
            })
            .with_source_basis(self.revision_basis()))
    }

    fn outline_selected_node(
        symbol: SymbolKey,
        children: &BTreeMap<SymbolKey, Vec<SymbolKey>>,
        visited: &mut BTreeSet<SymbolKey>,
        depth: usize,
        truncated: &mut bool,
    ) -> OutlineNode {
        const MAX_DEPTH: usize = 64;
        visited.insert(symbol);
        let mut nodes = Vec::new();
        for child in children.get(&symbol).into_iter().flatten() {
            if visited.contains(child) || depth >= MAX_DEPTH {
                *truncated = true;
                continue;
            }
            nodes.push(Self::outline_selected_node(
                *child,
                children,
                visited,
                depth + 1,
                truncated,
            ));
        }
        OutlineNode {
            symbol,
            children: nodes.into_boxed_slice(),
        }
    }

    /// Searches registered names and document text with stable row identity.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale query,
    /// [`LibraryError::InvalidQuery`] for empty search text, or
    /// [`LibraryError::View`] when the bounded snapshot cannot be constructed.
    pub fn search(&self, query: &Query) -> Result<ViewSnapshot, LibraryError> {
        self.search_ranked(query)
            .map(RankedSearchSnapshot::into_snapshot)
    }

    /// Searches while retaining relevance order separately from canonical
    /// view-relation order.
    ///
    /// # Errors
    ///
    /// Returns the same bounded query failures as the compatibility search.
    pub fn search_ranked(&self, query: &Query) -> Result<RankedSearchSnapshot, LibraryError> {
        self.check_basis(query.basis)?;
        if query.text.trim().is_empty() {
            return Err(LibraryError::InvalidQuery(
                "search text is empty".to_owned(),
            ));
        }
        let recipe = QueryPageRecipe::search(self.revision_root(), query).identity();
        self.work.record_seek();
        let window = query.window();
        let limit = usize::from(window.issued().get());
        let delivery = usize::from(window.delivery().get());
        let previous_recipe = QueryPageRecipe::search_predecessor(self.revision_root(), query)
            .ok_or(LibraryError::CursorMismatch)?
            .identity();
        let start =
            self.check_query_cursor(query.cursor, previous_recipe, limit, |offset, count| {
                self.arrangement.search_page(
                    query.text(),
                    offset,
                    count,
                    |id| self.view.row_ref(id),
                    &self.work,
                )
            })?;
        let page = self.arrangement.search_page(
            query.text(),
            start,
            delivery,
            |id| self.view.row_ref(id),
            &self.work,
        );
        if query.cursor.is_some() && page.ids.is_empty() {
            return Err(LibraryError::CursorMismatch);
        }
        let rows = page
            .ids
            .iter()
            .filter_map(|&id| self.view.row(id))
            .collect::<Vec<_>>();
        self.work.record_output(rows.len());
        let snapshot = self.snapshot_with_window(
            recipe,
            rows,
            if page.has_more {
                Some(self.cursor)
            } else {
                None
            },
            start,
            window,
        )?;
        Ok(RankedSearchSnapshot {
            snapshot,
            order: page.ids.into_boxed_slice(),
        })
    }

    /// Builds a search page from an owner-selected relevance prefix that
    /// includes the requested page and, when present, its first successor.
    ///
    /// This is the application-service seam for replaceable local search
    /// engines. Every identity is resolved from this library's immutable view;
    /// callers can choose relevance order but cannot inject row payloads or
    /// escape the request's root, recipe, page, or continuation bounds.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale query,
    /// [`LibraryError::InvalidQuery`] for invalid text or foreign/duplicate
    /// identities, and [`LibraryError::CursorMismatch`] for an invalid page.
    pub fn search_from_ranked_ids(
        &self,
        query: &Query,
        ranked_ids: &[RowId],
    ) -> Result<ViewSnapshot, LibraryError> {
        self.check_basis(query.basis)?;
        if query.text.trim().is_empty() {
            return Err(LibraryError::InvalidQuery(
                "search text is empty".to_owned(),
            ));
        }
        let mut unique = BTreeSet::new();
        if ranked_ids.len() > usize::try_from(self.view.row_count()).unwrap_or(usize::MAX)
            || ranked_ids
                .iter()
                .any(|id| !unique.insert(*id) || self.view.row_ref(*id).is_none())
        {
            return Err(LibraryError::InvalidQuery(
                "ranked search identities are not a subset of the selected view".to_owned(),
            ));
        }
        let recipe = QueryPageRecipe::search(self.revision_root(), query).identity();
        let window = query.window();
        let limit = usize::from(window.issued().get());
        let delivery = usize::from(window.delivery().get());
        let previous_recipe = QueryPageRecipe::search_predecessor(self.revision_root(), query)
            .ok_or(LibraryError::CursorMismatch)?
            .identity();
        let ranked_page = |start: usize, count: usize| {
            let mut ids = ranked_ids
                .iter()
                .copied()
                .skip(start)
                .take(count.saturating_add(1))
                .collect::<Vec<_>>();
            let has_more = ids.len() > count;
            ids.truncate(count);
            ArrangementPage { ids, has_more }
        };
        // Scores encode absolute rank, independent of prefix length. The
        // predecessor proof must resolve exactly these same scored rows.
        let ranked_row = |id, rank| {
            let mut row = self.view.row(id)?;
            row.score = Some(u32::MAX.checked_sub(u32::try_from(rank).ok()?)?);
            Some(row)
        };
        if u32::try_from(ranked_ids.len()).is_err() {
            return Err(LibraryError::InvalidQuery(
                "ranked search exceeds ordinal score space".to_owned(),
            ));
        }
        let start = self.check_query_cursor_with_rows(
            query.cursor,
            previous_recipe,
            limit,
            ranked_page,
            ranked_row,
        )?;
        let page = ranked_page(start, delivery);
        if query.cursor.is_some() && page.ids.is_empty() {
            return Err(LibraryError::CursorMismatch);
        }
        // A snapshot holds rows in key order; stable ordinal scores carry
        // relevance order across pages and cold reconstruction.
        let rows = page
            .ids
            .iter()
            .enumerate()
            .filter_map(|(at, &id)| ranked_row(id, start + at))
            .collect::<Vec<_>>();
        self.work.record_seek();
        self.work.record_output(rows.len());
        self.snapshot_with_window(
            recipe,
            rows,
            page.has_more.then_some(self.cursor),
            start,
            window,
        )
    }

    /// Returns one bounded graph neighborhood rooted at a declaration.
    ///
    /// The retained package/parent arrangement supplies the root's parent and
    /// direct children. No graph result is fabricated when the declaration is
    /// absent, and the result is always pinned to this library's source root.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::NotFound`] when the declaration is not in the
    /// accepted view, or [`LibraryError::View`] if the bounded snapshot cannot
    /// be constructed.
    pub fn graph(&self, query: GraphNeighborhoodQuery) -> Result<ViewSnapshot, LibraryError> {
        self.check_basis(query.basis())?;
        let symbol = query
            .resolve_symbol(&self.view)
            .ok_or(LibraryError::NotFound)?;
        self.work.record_seek();
        let root_id = RowId::Symbol(symbol);
        let root = self.view.row_ref(root_id).ok_or(LibraryError::NotFound)?;
        let mut ids = Vec::with_capacity(1 + usize::from(QueryLimit::MAX));
        ids.push(root_id);
        if let Some(package) = root.package {
            if let Some(parent) = root.parent {
                let parent_id = RowId::Symbol(parent);
                if self.arrangement.document(parent) && self.view.row_ref(parent_id).is_some() {
                    ids.push(parent_id);
                }
            }
            let remaining = usize::from(QueryLimit::MAX).saturating_sub(ids.len());
            let children = self
                .arrangement
                .children_page(package, Some(symbol), remaining);
            ids.extend(children.ids);
        }
        ids.sort_unstable();
        ids.dedup();
        let rows = ids
            .into_iter()
            .filter_map(|id| self.view.row(id))
            .collect::<Vec<_>>();
        self.work.record_output(rows.len());
        self.snapshot_for(b"graph", rows, None, None, None)
    }

    /// Builds a graph snapshot from compiler-proven semantic neighbor identities.
    ///
    /// The application owner may select graph edges from a richer authority such as a reopened
    /// compiler image. This method keeps row payload authority here: every supplied identity must
    /// be a member of this exact immutable view, the requested source must be present, and the
    /// fixed graph result bound is checked before any result root is built.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] when the query basis differs, the source or a target is absent,
    /// identities repeat, or the fixed result bound is exceeded.
    pub fn graph_from_semantic_ids(
        &self,
        query: GraphNeighborhoodQuery,
        ids: &[RowId],
    ) -> Result<ViewSnapshot, LibraryError> {
        self.check_basis(query.basis())?;
        let source = RowId::Symbol(
            query
                .resolve_symbol(&self.view)
                .ok_or(LibraryError::NotFound)?,
        );
        if ids.len() > 1 + usize::from(QueryLimit::MAX) || !ids.contains(&source) {
            return Err(LibraryError::InvalidQuery(
                "semantic graph identities violate the bounded source contract".to_owned(),
            ));
        }
        let mut unique = BTreeSet::new();
        let rows = ids
            .iter()
            .copied()
            .map(|id| {
                if !unique.insert(id) {
                    return Err(LibraryError::InvalidQuery(
                        "semantic graph contains a duplicate identity".to_owned(),
                    ));
                }
                self.view.row(id).ok_or_else(|| {
                    LibraryError::InvalidQuery(
                        "semantic graph identity is absent from the selected view".to_owned(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.work.record_seek();
        self.work.record_output(rows.len());
        // The query recipe identifies the public graph operation. Whether its
        // neighbors came from compiler links or the structural fallback is a
        // coverage/evidence fact, not a second client-visible query identity.
        self.snapshot_for(b"graph", rows, None, None, None)
    }

    /// Builds a graph snapshot from compiler-proven semantic edges.
    ///
    /// This is the typed companion to [`Self::graph_from_semantic_ids`]. Row
    /// payloads and canonical view commitments remain owned by the library;
    /// the owner contributes only edges selected from its admitted semantic
    /// image. The edge sidecar is explicitly versioned at the wire boundary
    /// and does not alter the committed view root.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] when an endpoint is outside this immutable
    /// view, the source is absent, an edge is repeated, or the bounded graph
    /// contract is exceeded.
    pub fn graph_from_semantic_relations(
        &self,
        query: GraphNeighborhoodQuery,
        relations: &[GraphRelation],
    ) -> Result<ViewSnapshot, LibraryError> {
        self.check_basis(query.basis())?;
        let source = RowId::Symbol(
            query
                .resolve_symbol(&self.view)
                .ok_or(LibraryError::NotFound)?,
        );
        let mut ids = BTreeSet::from([source]);
        let mut unique = BTreeSet::new();
        for relation in relations {
            if !unique.insert((relation.from, relation.to, relation.relation)) {
                return Err(LibraryError::InvalidQuery(
                    "semantic graph contains a duplicate typed relation".to_owned(),
                ));
            }
            if self.view.row_ref(relation.from).is_none()
                || self.view.row_ref(relation.to).is_none()
            {
                return Err(LibraryError::InvalidQuery(
                    "semantic graph relation endpoint is absent from the selected view".to_owned(),
                ));
            }
            ids.insert(relation.from);
            ids.insert(relation.to);
        }
        if ids.len() > 1 + usize::from(QueryLimit::MAX)
            || relations.len() > usize::from(QueryLimit::MAX)
        {
            return Err(LibraryError::InvalidQuery(
                "semantic graph relations violate the bounded source contract".to_owned(),
            ));
        }
        let rows = ids
            .into_iter()
            .map(|id| {
                self.view.row(id).ok_or_else(|| {
                    LibraryError::InvalidQuery(
                        "semantic graph identity is absent from the selected view".to_owned(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.work.record_seek();
        self.work.record_output(rows.len());
        let mut snapshot = self.snapshot_for(b"graph", rows, None, None, None)?;
        snapshot.graph_relations = Some(relations.to_vec().into_boxed_slice());
        Ok(snapshot)
    }

    /// Builds the find-references answer from compiler-verified occurrence
    /// facts against this library's immutable view.
    ///
    /// This is the application-service seam for the occurrence plane, shaped
    /// exactly like [`Library::graph_from_semantic_ids`]: the owner selects
    /// the evidence from a compiler authority, while row-payload authority
    /// stays here. Every site must name a declaration in this exact view, the
    /// target must be present, and the bounded result contract holds, so a
    /// reference answer can never cite a declaration the published view
    /// cannot show.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::NotFound`] when the target or a site is absent
    /// from the view, and [`LibraryError::InvalidQuery`] when the fact list
    /// or a resolved coordinate violates its bound.
    pub fn references(
        &self,
        target: &crate::ProductText,
        facts: &[ReferenceFact],
    ) -> Result<Box<[ReferenceRecord]>, LibraryError> {
        let target_present = self.view.row_refs().any(|row| row.label == target.as_str());
        if !target_present {
            return Err(LibraryError::NotFound);
        }
        self.reference_records(facts)
    }

    /// Projects reference sites for an actual selected native declaration.
    /// The execution owner keeps the requested source address separately.
    ///
    /// # Errors
    /// Refuses absent native targets, missing sites and invalid fact bounds.
    pub fn references_for_symbol(
        &self,
        target: SymbolKey,
        facts: &[ReferenceFact],
    ) -> Result<Box<[ReferenceRecord]>, LibraryError> {
        if self.view.row_ref(RowId::Symbol(target)).is_none() {
            return Err(LibraryError::NotFound);
        }
        self.reference_records(facts)
    }

    fn reference_records(
        &self,
        facts: &[ReferenceFact],
    ) -> Result<Box<[ReferenceRecord]>, LibraryError> {
        if facts.len() > crate::MAX_PRODUCT_ROWS {
            return Err(LibraryError::InvalidQuery(
                "reference facts exceed the bounded result contract".to_owned(),
            ));
        }
        let records = facts
            .iter()
            .map(|fact| {
                let label = self
                    .view
                    .row_label(RowId::Symbol(fact.site))
                    .ok_or(LibraryError::NotFound)?;
                Ok(ReferenceRecord {
                    site: crate::ProductText::new(label).map_err(|_| {
                        LibraryError::InvalidQuery(
                            "reference site coordinate violates the text bound".to_owned(),
                        )
                    })?,
                    target: fact.target.clone(),
                    relation: fact.relation,
                    evidence: fact.evidence.clone(),
                })
            })
            .collect::<Result<Vec<_>, LibraryError>>()?;
        self.work.record_seek();
        self.work.record_output(records.len());
        Ok(records.into_boxed_slice())
    }

    /// Reads one bounded graph-neighborhood page.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale revision,
    /// [`LibraryError::NotFound`] for an unknown symbol,
    /// [`LibraryError::CursorMismatch`] for a foreign continuation, or
    /// [`LibraryError::View`] when the bounded snapshot cannot be built.
    pub fn graph_page(
        &self,
        symbol: SymbolKey,
        page: PageRequest,
    ) -> Result<ProjectionPage, LibraryError> {
        self.graph_page_for_address(symbol, crate::SymbolAddress::canonical(symbol), page)
    }

    fn check_graph_address(
        &self,
        symbol: SymbolKey,
        address: crate::SymbolAddress,
    ) -> Result<(), LibraryError> {
        let row = self
            .view
            .row_ref(RowId::Symbol(symbol))
            .ok_or(LibraryError::NotFound)?;
        if address.matches(symbol) {
            return Ok(());
        }
        if address.is_selected() {
            return Err(LibraryError::NotFound);
        }
        let selected = if address.matches(crate::symbol_key(&row.label)) {
            // Producer-scoped exact labels without package metadata retain
            // their existing contract; source aliases always require ownership.
            if row.package.is_none()
                && self
                    .view
                    .unique_symbol_by_label(&row.label)
                    .is_some_and(|selected| selected.id == row.id)
            {
                return Ok(());
            }
            crate::resolve_selected_view_address(&self.view, &row.label)
        } else {
            let proof = crate::CapturedSourceCoordinate::from_row(row)
                .filter(|proof| proof.matches_address(address))
                .ok_or(LibraryError::NotFound)?;
            self.view
                .source_coordinate_key(proof.address_key().ok_or(LibraryError::NotFound)?)
        };
        match selected {
            crate::SelectedViewAddress::Declaration { row, .. }
                if row.id == RowId::Symbol(symbol) =>
            {
                Ok(())
            }
            _ => Err(LibraryError::InvalidQuery(
                "graph source address is absent or ambiguous in the selected view".to_owned(),
            )),
        }
    }

    /// Pages a typed symbol recovered by an execution owner from an admitted address.
    ///
    /// The original address binds the continuation recipe, while the selected
    /// symbol must exist in the exact page basis. This preserves canonical
    /// coordinate requests whose compiler-owned row key differs from their text key.
    ///
    /// # Errors
    /// Returns the same basis, membership, and continuation errors as `graph_page`.
    pub fn graph_page_for_address(
        &self,
        symbol: SymbolKey,
        address: crate::SymbolAddress,
        page: PageRequest,
    ) -> Result<ProjectionPage, LibraryError> {
        self.check_basis(page.basis())?;
        let root = self
            .view
            .row_ref(RowId::Symbol(symbol))
            .ok_or(LibraryError::NotFound)?;
        let window = page.window();
        let recipe_bytes = graph_page_recipe(address, window);
        let previous_recipe =
            QueryPageRecipe::graph_predecessor(self.revision_root(), address, page)
                .ok_or(LibraryError::CursorMismatch)?
                .identity();
        let limit = usize::from(window.issued().get());
        let delivery = usize::from(window.delivery().get());
        let cursor = page.continuation().map(crate::PageContinuation::cursor);
        let start = self.check_query_cursor(cursor, previous_recipe, limit, |offset, count| {
            self.graph_page_ids(root, symbol, offset, count)
        })?;
        self.check_graph_address(symbol, address)?;
        let result = self.graph_page_ids(root, symbol, start, delivery);
        self.projection_window_page(&recipe_bytes, &result, start, window)
    }

    /// Pages compiler-selected graph identities from this exact immutable view.
    ///
    /// # Errors
    /// Refuses foreign bases or cursors, missing source/neighbor rows, duplicate
    /// or reordered identities, and graphs exceeding the ordinary graph bound.
    pub fn graph_page_from_ids(
        &self,
        symbol: SymbolKey,
        address: crate::SymbolAddress,
        page: PageRequest,
        ids: &[RowId],
    ) -> Result<ProjectionPage, LibraryError> {
        self.check_basis(page.basis())?;
        if ids.len() > 1 + usize::from(QueryLimit::MAX)
            || ids.windows(2).any(|pair| pair[0] >= pair[1])
            || ids.binary_search(&RowId::Symbol(symbol)).is_err()
        {
            return Err(LibraryError::InvalidQuery(
                "graph page identities violate the selected neighborhood".to_owned(),
            ));
        }
        if ids.iter().any(|id| self.view.row_ref(*id).is_none()) {
            return Err(LibraryError::NotFound);
        }
        let window = page.window();
        let recipe_bytes = graph_page_recipe(address, window);
        let previous_recipe =
            QueryPageRecipe::graph_predecessor(self.revision_root(), address, page)
                .ok_or(LibraryError::CursorMismatch)?
                .identity();
        let limit = usize::from(window.issued().get());
        let delivery = usize::from(window.delivery().get());
        let select = |offset: usize, count: usize| ArrangementPage {
            ids: ids.iter().skip(offset).take(count).copied().collect(),
            has_more: offset.saturating_add(count) < ids.len(),
        };
        let start = self.check_query_cursor(
            page.continuation().map(crate::PageContinuation::cursor),
            previous_recipe,
            limit,
            select,
        )?;
        self.check_graph_address(symbol, address)?;
        self.projection_window_page(&recipe_bytes, &select(start, delivery), start, window)
    }

    fn graph_page_ids(
        &self,
        root: &Row,
        symbol: SymbolKey,
        start: usize,
        limit: usize,
    ) -> ArrangementPage {
        let mut prefix = vec![RowId::Symbol(symbol)];
        if let Some(parent) = root.parent {
            let id = RowId::Symbol(parent);
            if self.view.row_ref(id).is_some() {
                prefix.push(id);
            }
        }
        prefix.sort_unstable();
        prefix.dedup();
        let prefix_len = prefix.len();
        let mut ids = prefix
            .into_iter()
            .skip(start)
            .take(limit.saturating_add(1))
            .collect::<Vec<_>>();
        let mut children_have_more = false;
        if ids.len() <= limit
            && let Some(package) = root.package
        {
            let child_start = start.saturating_sub(prefix_len);
            let remaining = limit.saturating_add(1).saturating_sub(ids.len());
            let children =
                self.arrangement
                    .children_page_from(package, Some(symbol), child_start, remaining);
            children_have_more = children.has_more;
            ids.extend(children.ids);
        }
        let has_more = children_have_more || ids.len() > limit;
        ids.truncate(limit);
        ArrangementPage { ids, has_more }
    }

    fn projection_window_page(
        &self,
        recipe: &[u8],
        page: &ArrangementPage,
        start: usize,
        window: QueryWindow,
    ) -> Result<ProjectionPage, LibraryError> {
        let rows = page
            .ids
            .iter()
            .map(|&id| self.view.row(id).ok_or(LibraryError::NotFound))
            .collect::<Result<Vec<_>, _>>()?;
        let snapshot = self.snapshot_with_window(
            self.query_recipe(recipe, None),
            rows,
            page.has_more.then_some(self.cursor),
            start,
            window,
        )?;
        let terminal = snapshot.next.map_or(PageTerminal::Complete, |cursor| {
            PageTerminal::More(crate::PageContinuation::from_cursor(cursor))
        });
        Ok(ProjectionPage { snapshot, terminal, outline_order: None })
    }

    fn projection_page(
        &self,
        recipe: &[u8],
        page: &ArrangementPage,
        start: usize,
        limit: usize,
    ) -> Result<ProjectionPage, LibraryError> {
        let rows = page
            .ids
            .iter()
            .filter_map(|&id| self.view.row(id))
            .collect::<Vec<_>>();
        let snapshot = self.snapshot_for(
            recipe,
            rows,
            page.has_more.then_some(self.cursor),
            None,
            page.has_more.then_some(start.saturating_add(limit)),
        )?;
        let terminal = snapshot.next.map_or(PageTerminal::Complete, |cursor| {
            PageTerminal::More(crate::PageContinuation::from_cursor(cursor))
        });
        Ok(ProjectionPage { snapshot, terminal, outline_order: None })
    }
}

impl Library {
    fn check_basis(&self, basis: ViewRevision) -> Result<ViewStateRoot, LibraryError> {
        let expected = self.revision_root();
        if !basis.matches(expected) {
            return Err(LibraryError::WrongBasis {
                expected,
                observed: basis,
            });
        }
        Ok(expected)
    }

    pub(super) fn row_for_id(&self, id: RowId) -> Option<Row> {
        self.view.row(id)
    }

    pub(super) fn snapshot_for(
        &self,
        recipe: &[u8],
        rows: Vec<Row>,
        next: Option<Cursor>,
        read_manifest: Option<&ReadManifest>,
        next_offset: Option<usize>,
    ) -> Result<ViewSnapshot, LibraryError> {
        self.snapshot_with_recipe(
            self.query_recipe(recipe, read_manifest),
            rows,
            next,
            next_offset,
        )
    }

    fn snapshot_with_window(
        &self,
        id: ViewRecipeId,
        rows: Vec<Row>,
        next: Option<Cursor>,
        start: usize,
        window: QueryWindow,
    ) -> Result<ViewSnapshot, LibraryError> {
        let count = rows.len();
        if count > usize::from(window.delivery().get())
            || (window.is_windowed() && start > 0 && count == 0)
            || (next.is_some() && count != usize::from(window.delivery().get()))
        {
            return Err(LibraryError::CursorMismatch);
        }
        let offset = start
            .checked_add(count)
            .ok_or(LibraryError::CursorMismatch)?;
        let mut snapshot = self.snapshot_with_recipe(id, rows, next, next.map(|_| offset))?;
        if snapshot.root.row_count() != count as u64 {
            return Err(LibraryError::CursorMismatch);
        }
        if window.is_windowed()
            && let Some(cursor) = snapshot.next
        {
            let actual = QueryLimit::new(
                u16::try_from(snapshot.root.row_count())
                    .map_err(|_| LibraryError::CursorMismatch)?,
            )
            .ok_or(LibraryError::CursorMismatch)?;
            let span =
                QueryWindow::new(window.issued(), actual).ok_or(LibraryError::CursorMismatch)?;
            let position = crate::QueryPosition::windowed(
                u64::try_from(offset).map_err(|_| LibraryError::CursorMismatch)?,
                span,
            )
            .ok_or(LibraryError::CursorMismatch)?;
            snapshot.next = Some(cursor.with_query_position(position));
        }
        Ok(snapshot)
    }

    fn snapshot_with_recipe(
        &self,
        id: ViewRecipeId,
        rows: Vec<Row>,
        next: Option<Cursor>,
        next_offset: Option<usize>,
    ) -> Result<ViewSnapshot, LibraryError> {
        let next_offset = next_offset
            .map(u64::try_from)
            .transpose()
            .map_err(|_| LibraryError::CursorMismatch)?;
        let coverage = self.view.coverage.to_vec();
        let basis = self.revision_basis();
        let frontier = self.revision_frontier();
        let rows = Self::rows_at_basis(rows, basis);
        let root = if let Some(capability) = self.view.capability() {
            ViewRoot::new_checked(id, basis, frontier, rows, coverage, capability)?
        } else {
            ViewRoot::new_incomplete(id, basis, frontier, rows, coverage)?
        };
        let next = next.map(|cursor| {
            let next = Cursor::for_view(
                root.recipe,
                root.version,
                Frontier::new(
                    cursor.branch(),
                    cursor.log(),
                    cursor.schema(),
                    root.root,
                    cursor.sequence(),
                ),
            );
            next_offset.map_or(next, |offset| next.with_query_offset(offset))
        });
        Ok(ViewSnapshot {
            root,
            freshness: Freshness::Current,
            next,
            graph_relations: None,
            rich_graph: None,
        })
    }

    fn query_recipe(&self, recipe: &[u8], read_manifest: Option<&ReadManifest>) -> ViewRecipeId {
        projection_recipe(recipe, self.revision_root(), read_manifest)
    }

    fn check_query_cursor(
        &self,
        cursor: Option<Cursor>,
        recipe: ViewRecipeId,
        limit: usize,
        fetch: impl Fn(usize, usize) -> ArrangementPage,
    ) -> Result<usize, LibraryError> {
        self.check_query_cursor_with_rows(cursor, recipe, limit, fetch, |id, _| self.view.row(id))
    }

    fn check_query_cursor_with_rows(
        &self,
        cursor: Option<Cursor>,
        recipe: ViewRecipeId,
        limit: usize,
        fetch: impl Fn(usize, usize) -> ArrangementPage,
        row_at: impl Fn(RowId, usize) -> Option<Row>,
    ) -> Result<usize, LibraryError> {
        let Some(cursor) = cursor else {
            return Ok(0);
        };
        if cursor.recipe() != recipe
            || cursor.branch() != self.cursor.branch()
            || cursor.log() != self.cursor.log()
            || cursor.schema() != self.cursor.schema()
            || cursor.sequence() > self.cursor.sequence()
        {
            return Err(LibraryError::CursorMismatch);
        }
        let offset =
            usize::try_from(cursor.query_offset()).map_err(|_| LibraryError::CursorMismatch)?;
        if offset == 0 && cursor.version() == self.view.version && cursor.root() == self.view.root {
            return Ok(0);
        }
        let page_size = cursor
            .query_position()
            .predecessor_rows(
                QueryLimit::new(u16::try_from(limit).map_err(|_| LibraryError::CursorMismatch)?)
                    .ok_or(LibraryError::CursorMismatch)?,
            )
            .ok_or(LibraryError::CursorMismatch)?;
        if offset < page_size {
            return Err(LibraryError::CursorMismatch);
        }
        let previous_start = offset - page_size;
        let previous_page = fetch(previous_start, page_size);
        if previous_page.ids.len() != page_size {
            return Err(LibraryError::CursorMismatch);
        }
        let previous_rows = previous_page
            .ids
            .iter()
            .enumerate()
            .filter_map(|(at, &id)| row_at(id, previous_start + at))
            .collect::<Vec<_>>();
        let basis = self.revision_basis();
        let frontier = self.revision_frontier();
        let previous_rows = Self::rows_at_basis(previous_rows, basis);
        let expected = if let Some(capability) = self.view.capability() {
            ViewRoot::new_checked(
                recipe,
                basis,
                frontier,
                previous_rows,
                self.view.coverage.to_vec(),
                capability,
            )
            .map_err(LibraryError::View)?
        } else {
            ViewRoot::new_incomplete(
                recipe,
                basis,
                frontier,
                previous_rows,
                self.view.coverage.to_vec(),
            )
            .map_err(LibraryError::View)?
        };
        if cursor.version() != expected.version() || cursor.root() != expected.root() {
            return Err(LibraryError::CursorMismatch);
        }
        Ok(offset)
    }

    fn revision_basis(&self) -> crate::Basis {
        crate::Basis {
            root: self.view.root,
            ..self.view.basis
        }
    }

    fn revision_frontier(&self) -> Frontier {
        let basis = self.revision_basis();
        Frontier::new(
            basis.branch,
            basis.log,
            basis.schema,
            basis.root,
            self.view.frontier.sequence,
        )
    }

    fn rows_at_basis(mut rows: Vec<Row>, basis: crate::Basis) -> Vec<Row> {
        for row in &mut rows {
            row.basis = basis;
        }
        rows
    }
}
