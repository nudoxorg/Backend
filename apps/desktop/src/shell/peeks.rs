//! Peeks through W-Float's float layer: the shell's read models become
//! `facet::overlay::peek` cards. A card's content reads the store every
//! frame, so a peek opened before its page lands fills in when it does.

use super::kit::kind_of;
use crate::model::pages::{DocFragment, PackageRecord, PageKey, RecordSource, SignatureText, SymbolRef, TokenClass};
use crate::runtime::store::DataStore;
use facet::overlay::peek::{Peek, PackagePeek, SymbolPeek};
use facet::overlay::text::{Role, Sig};
use facet::overlay::{FloatKind, FloatRequest};
use gpui::{App, Bounds, ElementId, Entity, Pixels, SharedString, Window};

/// The float key a peek of `key` opens under.
pub(crate) fn float_key(key: &PageKey) -> ElementId {
    ElementId::Name(SharedString::from(format!("peek:{key}")))
}

/// A float request for the page `key`, anchored at `anchor`, drawn from
/// whatever the store holds for it at each frame.
pub(crate) fn request(key: PageKey, label: SharedString, anchor: Bounds<Pixels>, store: Entity<DataStore>) -> FloatRequest {
    let float = float_key(&key);
    FloatRequest::new(float, anchor, FloatKind::Peek, move |measure, window, cx| {
        let peek = peek_of(&key, &label, store.read(cx));
        facet::overlay::peek::content(peek)(measure, window, cx)
    })
}

/// The card for `key` as the store has it now.
fn peek_of(key: &PageKey, label: &SharedString, store: &DataStore) -> Peek {
    let symbol = match key {
        PageKey::Symbol(symbol) => symbol,
        // Dead end #16: the dossier is loaded (the shelf and the package
        // page already read it); the peek now shows it too, instead of
        // falling to an empty card with only the row's own label.
        PageKey::Package(package) => return package_peek(label, store.package(package).loaded_value()),
        _ => {
            return Peek::Symbol(SymbolPeek {
                name: label.clone(),
                ..SymbolPeek::default()
            });
        }
    };
    let page = store.symbol(symbol);
    let Some(page) = page.loaded_value() else {
        return Peek::Symbol(SymbolPeek {
            name: symbol.identity().name().to_owned().into(),
            place: where_of(symbol),
            path: symbol.identity().to_string().into(),
            ..SymbolPeek::default()
        });
    };
    let sentence = DocFragment::plain_text(&page.docs)
        .split_terminator(['.', '\n'])
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| SharedString::from(format!("{line}.")));
    Peek::Symbol(SymbolPeek {
        kind: Some(kind_of(page.identity.kind)),
        name: page.identity.name.to_string().into(),
        place: format!("{} in `{}`", page.identity.kind_name(), where_of(symbol)).into(),
        path: symbol.identity().to_string().into(),
        signature: page.signature.known().map(sig),
        sentence,
        uses: page.references.known().map(|sites| sites.len()),
        ..SymbolPeek::default()
    })
}

/// A package peek from its dossier: name, version, one sentence, and how
/// much of it your code reaches (dead end #16). A dossier not yet loaded
/// still shows the row's own label, same as a cold symbol peek.
fn package_peek(label: &SharedString, dossier: Option<&crate::model::pages::PackageDossier>) -> Peek {
    let Some(record) = dossier.and_then(|dossier| dossier.record.known()) else {
        return Peek::Package(PackagePeek { name: label.clone(), ..PackagePeek::default() });
    };
    let dossier = dossier.expect("a known record's dossier");
    Peek::Package(PackagePeek {
        name: record.name.to_string().into(),
        version: record.version.known().map_or_else(SharedString::default, |version| version.to_string().into()),
        registry: registry_of(record),
        sentence: record.description.known().map(|description| SharedString::from(description.to_string())),
        reach: dossier.dependents.known().map(|dependents| dependents.len()),
        ..PackagePeek::default()
    })
}

/// Where a package's facts came from, in words a reader already knows from
/// the shelf: your own manifest, or the ecosystem's registry.
fn registry_of(record: &PackageRecord) -> SharedString {
    match record.source {
        RecordSource::LocalManifest => "your project".into(),
        RecordSource::Registry => record.ecosystem.known().map_or_else(SharedString::default, |ecosystem| ecosystem.to_string().into()),
    }
}

fn where_of(symbol: &SymbolRef) -> SharedString {
    let identity = symbol.identity();
    let mut parts = Vec::new();
    if let Some(project) = identity.project() {
        parts.push(project.name().to_owned());
    }
    if let Some(path) = identity.path() {
        let stem = path.stem();
        if !stem.is_empty() && !matches!(stem, "lib" | "mod" | "main") {
            parts.push(stem.to_owned());
        }
    }
    parts.join("::").into()
}

/// A classified signature as the peek's one-line run.
fn sig(signature: &SignatureText) -> Sig {
    let mut out = Sig::new();
    for token in signature.tokens.iter() {
        let text = signature.token_text(token).to_owned();
        let role = match token.class {
            TokenClass::Keyword => Role::Keyword,
            TokenClass::Name | TokenClass::Type => Role::Type,
            TokenClass::Binding => Role::Param,
            TokenClass::Lifetime => Role::Macro,
            TokenClass::Literal => Role::Value,
            TokenClass::Punctuation | TokenClass::Text => Role::Punct,
        };
        out = out.span(text, role);
    }
    if signature.tokens.is_empty() {
        out = out.span(signature.text.to_string(), Role::Punct);
    }
    out
}

/// Whether the float layer shows a card for `key`.
pub(crate) fn is_open(key: &PageKey, window: &Window, cx: &mut App) -> bool {
    facet::overlay::float::is_open(&float_key(key), window, cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::pages::PackageRef;
    use crate::navigation::{OrbitRoute, Route};
    use crate::shell::tests::{PACKAGE, rig};

    /// Dead end #16: peeking a package row shows its dossier (name,
    /// version, one sentence, reach), not an empty card with only the
    /// row's own label.
    #[gpui::test]
    fn a_package_peek_shows_the_loaded_dossier(cx: &mut gpui::TestAppContext) {
        let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0);
        let package = PackageRef::parse(PACKAGE).expect("package");
        let key = PageKey::Package(package.clone());
        rig.graph.store.update(rig.cx, |store, cx| store.ensure(key.clone(), cx));
        rig.settle();
        let peek = rig.graph.store.read_with(rig.cx, |store, _| peek_of(&key, &"present".into(), store));
        let Peek::Package(card) = peek else { panic!("expected a package peek") };
        assert_eq!(card.name.as_ref(), "present");
        assert_eq!(card.version.as_ref(), "0.4.2");
        assert_eq!(card.registry.as_ref(), "your project", "a local manifest, not a registry release");
        assert_eq!(card.sentence.as_deref(), Some("How one symbol page reads."));
        assert_eq!(card.reach, None, "the fixture's dependents are unread");
    }

    /// A package peek opened before its dossier lands still shows the
    /// row's own label, exactly like a cold symbol peek.
    #[gpui::test]
    fn a_cold_package_peek_shows_only_its_label(cx: &mut gpui::TestAppContext) {
        let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0);
        let package = PackageRef::parse(PACKAGE).expect("package");
        let key = PageKey::Package(package);
        let peek = rig.graph.store.read_with(rig.cx, |store, _| peek_of(&key, &"present".into(), store));
        let Peek::Package(card) = peek else { panic!("expected a package peek") };
        assert_eq!(card.name.as_ref(), "present");
        assert_eq!(card.sentence, None);
    }
}
