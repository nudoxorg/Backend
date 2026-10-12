//! Borrowed source addresses of admitted declaration rows.

use crate::{Row, RowId};

/// A declaration label whose exact package prefix agrees with its owning key.
/// Projections can borrow this proof without repeating the owning package row.
#[derive(Clone, Copy, Debug)]
pub struct PackageQualifiedDeclaration<'row> {
    row: &'row Row,
    package_label: &'row str,
}

impl<'row> PackageQualifiedDeclaration<'row> {
    /// Checks the unique, nonempty package prefix against the actual owner key.
    #[must_use]
    pub fn from_row(row: &'row Row) -> Option<Self> {
        if row.label.len() > crate::MAX_ROW_IDENTITY_PREIMAGE_BYTES
            || !matches!(row.id, RowId::Symbol(_))
        {
            return None;
        }
        let package = row.package?;
        let mut prefixes = row.label.match_indices("::").filter_map(|(offset, _)| {
            let prefix = &row.label[..offset];
            (!prefix.is_empty()
                && offset + 2 < row.label.len()
                && crate::package_key(prefix) == package)
                .then_some(prefix)
        });
        let package_label = prefixes.next()?;
        if prefixes.next().is_some() {
            return None;
        }
        Some(Self { row, package_label })
    }

    /// Also checks the actual same-basis package row in the selected owner view.
    #[must_use]
    pub fn from_row_and_package(row: &'row Row, package: &'row Row) -> Option<Self> {
        if row.label.len() > crate::MAX_ROW_IDENTITY_PREIMAGE_BYTES
            || !matches!(row.id, RowId::Symbol(_))
            || package.label.is_empty()
            || package.id != RowId::Package(row.package?)
            || package.basis != row.basis
            || package.id != RowId::Package(crate::package_key(&package.label))
            || !row
                .label
                .strip_prefix(package.label.as_str())
                .and_then(|suffix| suffix.strip_prefix("::"))
                .is_some_and(|suffix| !suffix.is_empty())
        {
            return None;
        }
        Some(Self {
            row,
            package_label: &package.label,
        })
    }

    /// The actual admitted declaration, retaining its native identity.
    #[must_use]
    pub fn row(self) -> &'row Row {
        self.row
    }

    /// Exact verified owning package label prefix.
    #[must_use]
    pub fn package_label(self) -> &'row str {
        self.package_label
    }
}

/// Captured path, line, declaration name and verified owning package prefix.
/// This is an address proof, never a replacement for the row's native identity.
#[derive(Clone, Copy, Debug)]
pub struct CapturedSourceCoordinate<'row> {
    row: &'row Row,
    package_label: &'row str,
    name: &'row str,
    location: &'row crate::SourceLocation,
}

impl<'row> CapturedSourceCoordinate<'row> {
    /// Borrows a captured declaration whose label contains a unique package
    /// prefix authenticated by its actual package key. Graph projections need
    /// not repeat the package row alongside every neighborhood row.
    #[must_use]
    pub fn from_row(row: &'row Row) -> Option<Self> {
        let declaration = PackageQualifiedDeclaration::from_row(row)?;
        Self::from_parts(row, declaration.package_label(), row.source.captured()?)
    }

    /// Additionally checks the actual owning package row in a selected view.
    #[must_use]
    pub fn from_row_and_package(row: &'row Row, package: &'row Row) -> Option<Self> {
        let declaration = PackageQualifiedDeclaration::from_row_and_package(row, package)?;
        Self::from_parts(row, declaration.package_label(), row.source.captured()?)
    }

    fn from_parts(
        row: &'row Row,
        package_label: &'row str,
        location: &'row crate::SourceLocation,
    ) -> Option<Self> {
        let suffix = row.label.strip_prefix(package_label)?.strip_prefix("::")?;
        let name = if let Some(semantic) = suffix.strip_prefix("semantic::") {
            let (identity, name) = semantic.split_once("::")?;
            if identity.is_empty() {
                return None;
            }
            name
        } else {
            let site = suffix.strip_prefix(location.path())?.strip_prefix(':')?;
            let (line, name) = site.split_once("::")?;
            if canonical_line(line)? != location.start_line() {
                return None;
            }
            name
        };
        if name.is_empty() {
            return None;
        }
        Some(Self {
            row,
            package_label,
            name,
            location,
        })
    }

    /// Captured relative source path.
    #[must_use]
    pub fn path(self) -> &'row str {
        self.location.path()
    }
    /// Captured one-based line.
    #[must_use]
    pub fn line(self) -> u32 {
        self.location.start_line()
    }
    /// Actual declaration name retained by the row label.
    #[must_use]
    pub fn name(self) -> &'row str {
        self.name
    }
    /// Actual admitted row, including its native identity and basis.
    #[must_use]
    pub fn row(self) -> &'row Row {
        self.row
    }
    /// Canonical legacy source address, derived only from admitted row fields.
    #[must_use]
    pub fn coordinate(self) -> String {
        format!(
            "{}::{}:{}::{}",
            self.package_label,
            self.path(),
            self.line(),
            self.name
        )
    }
    /// Matches only the canonical source spelling (no alternate numeric syntax).
    #[must_use]
    pub fn matches(self, coordinate: &str) -> bool {
        coordinate
            .strip_prefix(self.package_label)
            .and_then(|suffix| suffix.strip_prefix("::"))
            .and_then(|suffix| suffix.strip_prefix(self.path()))
            .and_then(|suffix| suffix.strip_prefix(':'))
            .and_then(|suffix| suffix.split_once("::"))
            .is_some_and(|(line, name)| {
                canonical_line(line) == Some(self.line()) && name == self.name()
            })
    }
    /// Streams the canonical source address through the existing symbol schema.
    /// The result is only a lookup key; membership still borrows the actual row.
    #[must_use]
    pub fn address_key(self) -> Option<crate::SymbolKey> {
        use crate::canonical::SymbolSchema;
        use backend_version::{ObjectKeyHasher, Schema, SchemaIdentity};
        let mut digits = [0_u8; 10];
        let mut start = digits.len();
        let mut line = self.line();
        loop {
            start -= 1;
            digits[start] = b'0' + u8::try_from(line % 10).ok()?;
            line /= 10;
            if line == 0 {
                break;
            }
        }
        let parts = [
            self.package_label.as_bytes(),
            b"::",
            self.path().as_bytes(),
            b":",
            &digits[start..],
            b"::",
            self.name.as_bytes(),
        ];
        let length = parts
            .iter()
            .try_fold(0_usize, |sum, part| sum.checked_add(part.len()))?;
        if length > crate::MAX_ROW_IDENTITY_PREIMAGE_BYTES {
            return None;
        }
        let schema = SchemaIdentity::new(
            SymbolSchema::DOMAIN,
            SymbolSchema::TYPE,
            SymbolSchema::VERSION,
        );
        let mut hasher = ObjectKeyHasher::new(schema, length).ok()?;
        for part in parts {
            hasher.update(part).ok()?;
        }
        hasher.finish_key::<SymbolSchema>().ok()
    }
    /// Checks a canonical locator without turning its text hash into row identity.
    #[must_use]
    pub fn matches_address(self, address: crate::SymbolAddress) -> bool {
        !address.is_selected() && self.address_key().is_some_and(|key| address.matches(key))
    }
}

/// How a public coordinate matched one row in the selected view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressMatchKind {
    /// The coordinate is the row's exact selected label.
    ExactLabel,
    /// The legacy project/path/line/name spelling uniquely names a captured site.
    LegacySourceSite,
}

/// Typed resolution of one public coordinate against a selected view.
///
/// Declaration rows require their owning package row to bind the requested
/// package prefix. Alias resolution additionally requires a unique captured
/// source site. The caller can therefore distinguish a package operand, an
/// absent declaration, and an ambiguous coordinate without treating a
/// matching name or relative path as authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectedViewAddress<'view> {
    /// A declaration row, with the spelling that selected it.
    Declaration {
        /// Borrowed row from this exact immutable view.
        row: &'view Row,
        /// Whether the request used its current label or a legacy source site.
        matched_as: AddressMatchKind,
    },
    /// An exact selected package row, which is not a declaration.
    Package {
        /// Borrowed package row from this exact immutable view.
        row: &'view Row,
    },
    /// An exact selected row of another type.
    Other {
        /// Borrowed row from this exact immutable view.
        row: &'view Row,
    },
    /// No exact row or uniquely proven legacy source site matches.
    Absent,
    /// Multiple selected rows or package-prefix parses match.
    Ambiguous,
}

impl<'view> SelectedViewAddress<'view> {
    /// Returns a declaration row, translating non-declaration coordinates to
    /// the public query failure used by document and graph commands.
    ///
    /// # Errors
    /// Refuses package, non-declaration and ambiguous addresses.
    pub fn declaration(self) -> Result<Option<&'view Row>, crate::CommandFailure> {
        match self {
            Self::Declaration { row, .. } => Ok(Some(row)),
            Self::Package { .. } => Err(crate::CommandFailure::InvalidQuery(
                "coordinate names a package, not a declaration".to_owned(),
            )),
            Self::Other { .. } => Err(crate::CommandFailure::InvalidQuery(
                "coordinate does not name a declaration".to_owned(),
            )),
            Self::Absent => Ok(None),
            Self::Ambiguous => Err(crate::CommandFailure::InvalidQuery(
                "coordinate is ambiguous in the selected view".to_owned(),
            )),
        }
    }
}

/// Resolves a public coordinate using exact labels first, then a package-bound
/// legacy source coordinate.
///
/// An exact declaration label is accepted only when its row's package key
/// resolves to an actual package row and that row's exact label prefixes the
/// coordinate. Legacy spelling is considered only when the selected view
/// contains that same package row; its exact label prefix is matched before
/// the relative path, one-based line, and declaration name are compared with
/// captured source locations. No row is selected by name or path alone, and
/// repeated matches remain ambiguous.
pub fn resolve_selected_view_address<'view>(
    view: &'view crate::ViewRoot,
    coordinate: &str,
) -> SelectedViewAddress<'view> {
    if coordinate.is_empty() || coordinate.len() > crate::MAX_ROW_IDENTITY_PREIMAGE_BYTES {
        return SelectedViewAddress::Absent;
    }
    if view.label_is_ambiguous(coordinate) {
        return SelectedViewAddress::Ambiguous;
    }
    if let Some(row) = view.row_by_label(coordinate) {
        return match row.id {
            RowId::Symbol(_) => {
                let Some(package) = row.package else {
                    return SelectedViewAddress::Absent;
                };
                let Some(package_row) = view.row_ref(RowId::Package(package)) else {
                    return SelectedViewAddress::Absent;
                };
                if PackageQualifiedDeclaration::from_row_and_package(row, package_row).is_none() {
                    return SelectedViewAddress::Absent;
                }
                if view
                    .unique_symbol_by_label(coordinate)
                    .is_some_and(|symbol| symbol.id == row.id)
                {
                    SelectedViewAddress::Declaration {
                        row,
                        matched_as: AddressMatchKind::ExactLabel,
                    }
                } else {
                    SelectedViewAddress::Ambiguous
                }
            }
            RowId::Package(package) if crate::package_key(coordinate) == package => {
                SelectedViewAddress::Package { row }
            }
            _ => SelectedViewAddress::Other { row },
        };
    }

    // The immutable root caches only lookup keys and unique/ambiguous row IDs.
    // The selected candidate is still checked against the exact borrowed tuple.
    view.source_coordinate_address(coordinate)
}

/// Resolves a document address to its exact admitted symbol row.
///
/// A retained symbol without local package membership can still report its
/// actual document and source availability. Its complete, unique label must
/// exist in this immutable view; a name, path, or parsed key is insufficient.
/// This does not grant declaration or owning-package authority. Rows claiming
/// a package and legacy source aliases keep the stricter declaration proof.
///
/// # Errors
/// Refuses ambiguous labels and coordinates naming a non-symbol row.
pub fn resolve_selected_document_row<'view>(
    view: &'view crate::ViewRoot,
    coordinate: &str,
) -> Result<Option<&'view Row>, crate::CommandFailure> {
    if coordinate.is_empty() || coordinate.len() > crate::MAX_ROW_IDENTITY_PREIMAGE_BYTES {
        return Ok(None);
    }
    if view.label_is_ambiguous(coordinate) {
        return SelectedViewAddress::Ambiguous.declaration();
    }
    if let Some(row) = view.unique_symbol_by_label(coordinate)
        && row.package.is_none()
    {
        return Ok(Some(row));
    }
    resolve_selected_view_address(view, coordinate).declaration()
}

fn canonical_line(line: &str) -> Option<u32> {
    if line.is_empty() || line.starts_with('0') || !line.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    line.parse().ok()
}
