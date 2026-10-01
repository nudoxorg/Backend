//! Hops between declarations: the page's mark and its name are the shared
//! elements the reader flies (W-Glyph), under the keys every row that opens
//! the page carries, so a hop from a row or from another page keeps its flight.

use super::page_tests::{label, route};
use crate::model::pages::SymbolRef;
use crate::runtime::reads::ReadPool;
use crate::shell::kit::shared_id;
use crate::shell::tests::{PACKAGE, rig_with_reads};
use gpui::TestAppContext;

/// The symbol a pinned page is about.
fn symbol(file: &str, line: u32, name: &str) -> SymbolRef {
    SymbolRef::new(&label(file, line, name)).expect("a symbol")
}

#[gpui::test]
fn a_page_paints_its_mark_and_its_name_as_the_shared_elements_a_hop_flies(cx: &mut TestAppContext) {
    let pool = ReadPool::start(2, |_| super::page_tests::Pinned).expect("pinned pool");
    let mut rig = rig_with_reads(
        cx,
        Some(route("de.rs", 2709, "from_str")),
        1440.0,
        900.0,
        pool,
    );
    rig.settle();
    rig.repaint();
    let from_str = symbol("de.rs", 2709, "from_str");
    let mark = rig
        .cx
        .update(|window, cx| facet::motion::shared::last_bounds(shared_id(&from_str), window, cx))
        .expect("the mark is a shared element under the declaration's key");
    let name = rig
        .cx
        .update(|window, cx| {
            facet::motion::shared::last_bounds(
                facet::anatomy::page::title_key(from_str.as_str()),
                window,
                cx,
            )
        })
        .expect("the name is a shared element under the title key");
    assert!(
        mark.size.width > gpui::px(8.0) && name.size.width > gpui::px(40.0),
        "both are painted: {mark:?} {name:?}"
    );
    assert!(
        mark.origin.y < name.origin.y + name.size.height,
        "the mark sits with the name, not below it"
    );
    // A hop to another declaration puts the same two shared elements under the new keys.
    let value = symbol("mod.rs", 116, "Value");
    rig.go(crate::navigation::Intent::Navigate(route(
        "mod.rs", 116, "Value",
    )));
    rig.repaint();
    assert!(
        rig.cx
            .update(|window, cx| facet::motion::shared::last_bounds(shared_id(&value), window, cx))
            .is_some(),
        "the new page's mark is under its own key ({PACKAGE})"
    );
    assert!(
        rig.cx
            .update(|window, cx| facet::motion::shared::last_bounds(
                facet::anatomy::page::title_key(value.as_str()),
                window,
                cx
            ))
            .is_some(),
        "and its name under its title key"
    );
}
