use super::reply::{BasisWire, basis_from_wire, basis_to_wire};
use super::reply_admission::CoverageAdmission;
use super::{EmptyWire, TextWire, WireCertificate, WireSchema};
use crate::canonical::{PackageSchema, SymbolSchema, encode_id};
use crate::{
    CoverageCapability, Document, Fragment, Outline, OutlineExtent, OutlineNode,
    SourceAvailability, SourceExcerpt, SourceExcerptExtent, SourceLocation,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocumentWire {
    symbol: String,
    basis: String,
    source: Option<BasisWire>,
    fragments: Vec<FragmentWire>,
    signature: Option<String>,
    location: SourceAvailabilityWire,
    #[serde(default = "source_excerpt_not_captured")]
    excerpt: SourceExcerptWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FragmentWire {
    Text(TextWire),
    Code(TextWire),
    Link(LinkWire),
    Break(EmptyWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceAvailabilityWire {
    Captured(SourceLocationWire),
    NotCaptured,
    NotHydrated,
    Unconfigured,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceExcerptWire {
    Captured(SourceExcerptCapturedWire),
    NotCaptured,
    NotHydrated,
    Unconfigured,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceExcerptCapturedWire {
    text: String,
    extent: SourceExcerptExtentWire,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceExcerptExtentWire {
    Complete,
    Truncated,
}

pub(crate) fn source_excerpt_not_captured() -> SourceExcerptWire {
    SourceExcerptWire::NotCaptured
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceLocationWire {
    path: String,
    start_line: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinkWire {
    pub(crate) label: String,
    pub(crate) target: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutlineWire {
    package: String,
    basis: String,
    source: Option<BasisWire>,
    root: OutlineNodeWire,
    additional_roots: Vec<OutlineNodeWire>,
    extent: OutlineExtentWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
enum OutlineExtentWire {
    Complete(EmptyWire),
    Truncated(EmptyWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutlineNodeWire {
    symbol: String,
    children: Vec<OutlineNodeWire>,
}

pub(crate) fn document_to_wire(document: &Document) -> DocumentWire {
    DocumentWire {
        symbol: encode_id(document.symbol.as_bytes()),
        basis: encode_id(document.basis().as_bytes()),
        source: document.source_basis().map(basis_to_wire),
        fragments: document.fragments.iter().map(fragment_to_wire).collect(),
        signature: document.signature.clone(),
        location: source_availability_to_wire(&document.location),
        excerpt: source_excerpt_to_wire(&document.excerpt),
    }
}

pub(crate) fn fragment_to_wire(fragment: &Fragment) -> FragmentWire {
    match fragment {
        Fragment::Text(text) => FragmentWire::Text(TextWire { text: text.clone() }),
        Fragment::Code(text) => FragmentWire::Code(TextWire { text: text.clone() }),
        Fragment::Link { label, target } => FragmentWire::Link(LinkWire {
            label: label.clone(),
            target: encode_id(target.as_bytes()),
        }),
        Fragment::Break => FragmentWire::Break(EmptyWire {}),
    }
}

pub(crate) fn fragment_from_wire_with_capability(
    fragment: FragmentWire,
    certificate: &WireCertificate,
    capability: Option<&CoverageCapability>,
) -> Result<Fragment, String> {
    Ok(match fragment {
        FragmentWire::Text(value) => Fragment::Text(value.text),
        FragmentWire::Code(value) => Fragment::Code(value.text),
        FragmentWire::Link(value) => Fragment::Link {
            label: value.label,
            target: match capability {
                Some(capability) => certificate.row_identity_or_key_or_producer::<SymbolSchema>(
                    WireSchema::Symbol,
                    &value.target,
                    capability,
                )?,
                None => certificate
                    .row_identity_or_key_value::<SymbolSchema>(WireSchema::Symbol, &value.target)?,
            },
        },
        FragmentWire::Break(_) => Fragment::Break,
    })
}

pub(crate) fn source_availability_to_wire(source: &SourceAvailability) -> SourceAvailabilityWire {
    match source {
        SourceAvailability::Captured(location) => {
            SourceAvailabilityWire::Captured(SourceLocationWire {
                path: location.path().to_owned(),
                start_line: location.start_line(),
            })
        }
        SourceAvailability::NotCaptured => SourceAvailabilityWire::NotCaptured,
        SourceAvailability::NotHydrated => SourceAvailabilityWire::NotHydrated,
        SourceAvailability::Unconfigured => SourceAvailabilityWire::Unconfigured,
    }
}

pub(crate) fn source_availability_from_wire(
    source: SourceAvailabilityWire,
) -> Result<SourceAvailability, String> {
    Ok(match source {
        SourceAvailabilityWire::Captured(location) => {
            SourceAvailability::Captured(SourceLocation::new(location.path, location.start_line)?)
        }
        SourceAvailabilityWire::NotCaptured => SourceAvailability::NotCaptured,
        SourceAvailabilityWire::NotHydrated => SourceAvailability::NotHydrated,
        SourceAvailabilityWire::Unconfigured => SourceAvailability::Unconfigured,
    })
}

pub(crate) fn source_excerpt_to_wire(source: &SourceExcerpt) -> SourceExcerptWire {
    match source {
        SourceExcerpt::Captured { text, extent } => {
            SourceExcerptWire::Captured(SourceExcerptCapturedWire {
                text: text.to_string(),
                extent: match extent {
                    SourceExcerptExtent::Complete => SourceExcerptExtentWire::Complete,
                    SourceExcerptExtent::Truncated => SourceExcerptExtentWire::Truncated,
                },
            })
        }
        SourceExcerpt::NotCaptured => SourceExcerptWire::NotCaptured,
        SourceExcerpt::NotHydrated => SourceExcerptWire::NotHydrated,
        SourceExcerpt::Unconfigured => SourceExcerptWire::Unconfigured,
    }
}

pub(crate) fn source_excerpt_from_wire(source: SourceExcerptWire) -> Result<SourceExcerpt, String> {
    match source {
        SourceExcerptWire::Captured(value) => SourceExcerpt::captured(
            &value.text,
            match value.extent {
                SourceExcerptExtentWire::Complete => SourceExcerptExtent::Complete,
                SourceExcerptExtentWire::Truncated => SourceExcerptExtent::Truncated,
            },
        ),
        SourceExcerptWire::NotCaptured => Ok(SourceExcerpt::NotCaptured),
        SourceExcerptWire::NotHydrated => Ok(SourceExcerpt::NotHydrated),
        SourceExcerptWire::Unconfigured => Ok(SourceExcerpt::Unconfigured),
    }
}

pub(crate) fn document_from_wire_with_admission<A: CoverageAdmission>(
    value: DocumentWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<Document, String> {
    let (basis, source, capability) = content_basis_from_wire(
        &value.basis,
        value.source.as_ref(),
        certificate,
        admission,
        "document",
    )?;
    let fragments = value
        .fragments
        .into_iter()
        .map(|fragment| {
            fragment_from_wire_with_capability(fragment, certificate, capability.as_ref())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let symbol = match capability.as_ref() {
        Some(capability) => certificate.row_identity_or_key_or_producer::<SymbolSchema>(
            WireSchema::Symbol,
            &value.symbol,
            capability,
        )?,
        None => certificate
            .row_identity_or_key_value::<SymbolSchema>(WireSchema::Symbol, &value.symbol)?,
    };
    Ok(Document {
        symbol,
        basis,
        source,
        fragments: fragments.into_boxed_slice(),
        signature: value.signature,
        location: source_availability_from_wire(value.location)?,
        excerpt: source_excerpt_from_wire(value.excerpt)?,
    })
}

pub(crate) fn outline_to_wire(outline: &Outline) -> OutlineWire {
    OutlineWire {
        package: encode_id(outline.package.as_bytes()),
        basis: encode_id(outline.basis().as_bytes()),
        source: outline.source_basis().map(basis_to_wire),
        root: outline_node_to_wire(&outline.root),
        additional_roots: outline
            .additional_roots
            .iter()
            .map(outline_node_to_wire)
            .collect(),
        extent: match outline.extent {
            OutlineExtent::Complete => OutlineExtentWire::Complete(EmptyWire {}),
            OutlineExtent::Truncated => OutlineExtentWire::Truncated(EmptyWire {}),
        },
    }
}

pub(crate) fn outline_from_wire_with_admission<A: CoverageAdmission>(
    value: OutlineWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<Outline, String> {
    let (basis, source, capability) = content_basis_from_wire(
        &value.basis,
        value.source.as_ref(),
        certificate,
        admission,
        "outline",
    )?;
    let additional_roots = value
        .additional_roots
        .into_iter()
        .map(|node| outline_node_from_wire_with_capability(node, certificate, capability.as_ref()))
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    let package = match capability.as_ref() {
        Some(capability) => certificate.row_identity_or_key_or_producer::<PackageSchema>(
            WireSchema::Package,
            &value.package,
            capability,
        )?,
        None => certificate
            .row_identity_or_key_value::<PackageSchema>(WireSchema::Package, &value.package)?,
    };
    Ok(Outline {
        package,
        basis,
        source,
        root: outline_node_from_wire_with_capability(value.root, certificate, capability.as_ref())?,
        additional_roots,
        extent: match value.extent {
            OutlineExtentWire::Complete(_) => OutlineExtent::Complete,
            OutlineExtentWire::Truncated(_) => OutlineExtent::Truncated,
        },
    })
}

fn content_basis_from_wire<A: CoverageAdmission>(
    encoded_basis: &str,
    source: Option<&BasisWire>,
    certificate: &WireCertificate,
    admission: &A,
    content_kind: &str,
) -> Result<
    (
        crate::ViewStateRoot,
        Option<crate::Basis>,
        Option<CoverageCapability>,
    ),
    String,
> {
    let Some(source) = source else {
        let basis = certificate
            .root_value::<crate::ViewRelation>(WireSchema::ViewRelation, encoded_basis)?;
        return Ok((basis, None, None));
    };
    let capability = admission.admit(certificate, super::reply::basis_object(source))?;
    let source = match capability.as_ref() {
        Some(capability) => {
            super::reply::basis_from_wire_with_capability(source, certificate, capability)?
        }
        None => basis_from_wire(source, certificate)?,
    };
    if encode_id(source.root.as_bytes()) != encoded_basis {
        return Err(format!(
            "{content_kind} source basis does not match its root"
        ));
    }
    Ok((source.root, Some(source), capability))
}

pub(crate) fn outline_node_to_wire(node: &OutlineNode) -> OutlineNodeWire {
    OutlineNodeWire {
        symbol: encode_id(node.symbol.as_bytes()),
        children: node.children.iter().map(outline_node_to_wire).collect(),
    }
}

fn outline_node_from_wire_with_capability(
    value: OutlineNodeWire,
    certificate: &WireCertificate,
    capability: Option<&CoverageCapability>,
) -> Result<OutlineNode, String> {
    let symbol = match capability {
        Some(capability) => certificate.row_identity_or_key_or_producer::<SymbolSchema>(
            WireSchema::Symbol,
            &value.symbol,
            capability,
        )?,
        None => certificate
            .row_identity_or_key_value::<SymbolSchema>(WireSchema::Symbol, &value.symbol)?,
    };
    Ok(OutlineNode {
        symbol,
        children: value
            .children
            .into_iter()
            .map(|child| outline_node_from_wire_with_capability(child, certificate, capability))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice(),
    })
}
