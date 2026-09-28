//! Typed page answers, derived from captured facts. Source spelling is an
//! input to this projection, never the default presentation.

use crate::model::pages::{DocFragment, DocSection, SectionKind};
use std::rc::Rc;
use std::sync::Arc;

pub(super) struct WorldPacket {
    pub relations: Vec<facet::semantics::relations::rows::RelationRow>,
    pub failure: Option<facet::semantics::fails::Section>,
    pub callable_failure: Option<facet::semantics::fails::KindsLine>,
}

#[derive(Default)]
struct Packets {
    names: Vec<(Arc<facet::graph::World>, Rc<facet::semantics::names::Names>)>,
    pages: Vec<(Arc<facet::graph::World>, facet::graph::NodeId, Rc<WorldPacket>)>,
}
impl gpui::Global for Packets {}

/// Immutable graph projection, cached by the actual world object and node.
/// Pointer interactions and folds only borrow this packet. Keeping its world
/// alive makes pointer reuse impossible; both caches are bounded.
pub(super) fn packet(anatomy: &crate::runtime::fixture_world::Anatomy, cx: &mut gpui::App) -> Rc<WorldPacket> {
    let cache = cx.default_global::<Packets>();
    if let Some(at) = cache.pages.iter().position(|(world, node, _)| Arc::ptr_eq(world, &anatomy.world) && *node == anatomy.node) {
        let entry = cache.pages.remove(at);
        let packet = entry.2.clone(); cache.pages.push(entry); return packet;
    }
    let names = if let Some((_, names)) = cache.names.iter().find(|(world, _)| Arc::ptr_eq(world, &anatomy.world)) { names.clone() }
        else { let names = Rc::new(facet::semantics::names::Names::new(&anatomy.world));
            cache.names.push((anatomy.world.clone(), names.clone()));
            if cache.names.len() > 3 { cache.names.remove(0); } names };
    let engine = facet::semantics::fails::Fails::new(&anatomy.world, &names);
    let packet = Rc::new(WorldPacket {
        relations: facet::semantics::relations::rows::rows(&anatomy.world, anatomy.node, facet::semantics::relations::relations_of(&anatomy.world, anatomy.node)),
        failure: engine.how_it_fails(anatomy.node),
        callable_failure: engine.kinds_line(anatomy.node),
    });
    cache.pages.push((anatomy.world.clone(), anatomy.node, packet.clone()));
    if cache.pages.len() > 48 { cache.pages.remove(0); }
    packet
}

pub(super) const fn is_failure(kind: SectionKind) -> bool {
    matches!(kind, SectionKind::Errors | SectionKind::Panics | SectionKind::Safety)
}

pub(super) fn failure_words(section: &DocSection) -> String {
    let mut words = DocFragment::plain_text(&section.body);
    for entry in section.entries.iter() {
        if !words.is_empty() { words.push('\n'); }
        words.push_str(&entry.subject);
        words.push_str(": ");
        words.push_str(&DocFragment::plain_text(&entry.body));
    }
    words
}
