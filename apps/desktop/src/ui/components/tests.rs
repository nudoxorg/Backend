use super::{ActionMetadata, ActionRole, ActionState, ActionTree, SemanticBounds, Weight};
use gpui::{Bounds, point, px, size};

#[test]
fn action_tree_rejects_duplicate_or_empty_contracts_and_keeps_disabled_actions() {
    let mut tree = ActionTree::default();
    let mut registration = tree.registrar();
    assert!(registration.record(
        ActionMetadata::new("search", "Search packages", ActionRole::Search).shortcut("⌘K")
    ));
    assert!(!registration.record(ActionMetadata::new("search", "Again", ActionRole::Search,)));
    assert!(!registration.record(ActionMetadata::new("", "Missing id", ActionRole::Button,)));
    assert!(
        registration.record(
            ActionMetadata::new("settings", "Settings", ActionRole::Setting).enabled(false)
        )
    );
    drop(registration);
    assert_eq!(tree.len(), 2);
    assert_eq!(tree.focusable().count(), 1);
    assert_eq!(tree.iter().count(), 2);
}

#[test]
fn action_roles_are_explicit_and_button_variants_remain_total() {
    assert_eq!(ActionRole::Button, ActionRole::Button);
    assert_ne!(ActionRole::TextInput, ActionRole::Search);
    assert_ne!(Weight::Primary, Weight::Quiet);
    assert_ne!(Weight::Primary, Weight::Regular);
    assert_ne!(Weight::Regular, Weight::Quiet);
}

#[test]
fn frames_track_final_state_and_remove_stale_controls() {
    let frames = super::ActionFrames::default();
    let home = frames.begin_id(11, "home");
    frames.register(
        home,
        super::ActionMetadata::new("submit", "Submit", ActionRole::Button)
            .enabled(false)
            .visible(true),
    );
    let first = frames.snapshot_id(11);
    let action = first.iter().next().expect("disabled action is retained");
    assert!(!action.is_enabled());
    assert!(action.is_visible());
    assert_eq!(action.focus_order(), 0);

    frames.begin_id(11, "settings");
    let second = frames.snapshot_id(11);
    assert_eq!(second.route().as_ref(), "settings");
    assert_eq!(second.len(), 0);
    assert!(second.revision() > first.revision());
}

#[test]
fn frames_are_separate_per_window_and_keep_input_search_roles() {
    let frames = super::ActionFrames::default();
    let search = frames.begin_id(21, "search");
    frames.register(
        search,
        super::ActionMetadata::new("query", "Query", ActionRole::TextInput)
            .focused(true)
            .with_value("serde"),
    );
    frames.register(
        search,
        super::ActionMetadata::new("package-search", "Search packages", ActionRole::Search),
    );
    let other_window = frames.begin_id(22, "other-window");
    frames.register(
        other_window,
        super::ActionMetadata::new("settings", "Settings", ActionRole::Setting),
    );
    let first = frames.snapshot_id(21);
    let second = frames.snapshot_id(22);
    assert_eq!(
        first.iter().map(|action| action.role()).collect::<Vec<_>>(),
        [ActionRole::TextInput, ActionRole::Search,]
    );
    assert_eq!(
        second.iter().next().map(|action| action.role()),
        Some(ActionRole::Setting)
    );
    assert_eq!(first.route().as_ref(), "search");
    assert_eq!(second.route().as_ref(), "other-window");
    let query = first.iter().next().expect("input action");
    assert!(query.is_focused());
    assert_eq!(query.value().map(|value| value.as_ref()), Some("serde"));
}

#[test]
fn retained_frame_tokens_cannot_register_into_a_new_frame() {
    let frames = super::ActionFrames::default();
    let old = frames.begin_id(31, "home");
    let current = frames.begin_id(31, "settings");
    frames.register(
        old,
        super::ActionMetadata::new("stale", "Stale", ActionRole::Button),
    );
    assert_eq!(frames.snapshot_id(31).len(), 0);
    frames.register(
        current,
        super::ActionMetadata::new("current", "Current", ActionRole::Button),
    );
    assert_eq!(frames.snapshot_id(31).len(), 1);
}

#[test]
fn measured_registry_keeps_real_rectangles_and_rendered_focus_owner() {
    let frames = super::ActionFrames::default();
    let token = frames.begin_id(35, "home");
    frames.register(
        token,
        ActionMetadata::new("first", "First", ActionRole::Button).focused(true),
    );
    frames.register(
        token,
        ActionMetadata::new("second", "Second", ActionRole::Button),
    );
    frames.record_bounds(
        token,
        "first".into(),
        Bounds::new(point(px(12.0), px(20.0)), size(px(80.0), px(44.0))),
    );
    frames.record_bounds(
        token,
        "second".into(),
        Bounds::new(point(px(104.0), px(20.0)), size(px(96.0), px(44.0))),
    );

    let bounds = frames.snapshot_bounds_id(35);
    assert_eq!(
        bounds.get("first").copied(),
        Some(SemanticBounds::Logical {
            x: 12,
            y: 20,
            width: 80,
            height: 44,
        })
    );
    assert_ne!(bounds.get("first"), bounds.get("second"));
    assert_eq!(frames.snapshot_focus_order_id(35).get("first"), Some(&0));
    assert_eq!(frames.snapshot_focus_order_id(35).get("second"), Some(&1));
    assert_eq!(frames.snapshot_focus_owner_id(35).as_deref(), Some("first"));
}

#[test]
fn focus_order_reflects_a_late_registered_dialog_control_after_finalize() {
    // A CE `Dialog`'s footer is built eagerly, but its own body is built by
    // a lazy `content` closure that GPUI does not run until the real
    // prepaint pass, after the frame's other controls (see `render_root` in
    // `apps/desktop/src/views/mod.rs`). The add-project dialog's own text
    // input registers exactly this way, so its focus order used to freeze at
    // whatever raw registration index it happened to get, instead of its
    // real position among the dialog's other controls.
    let frames = super::ActionFrames::default();
    let token = frames.begin_id(42, "home");
    frames.register(
        token,
        ActionMetadata::new(
            "add-project-dialog",
            "Add a local project",
            ActionRole::Dialog,
        ),
    );
    frames.register(
        token,
        ActionMetadata::new("add-project-browse", "Browse…", ActionRole::Button),
    );
    // Recording bounds for the eagerly-built footer button happens before
    // the dialog's lazy content has registered anything at all.
    frames.record_bounds(
        token,
        "add-project-browse".into(),
        Bounds::new(point(px(0.0), px(0.0)), size(px(80.0), px(32.0))),
    );
    // The dialog's lazy content registers its input only now, simulating
    // `Dialog::content`'s deferred builder.
    frames.register(
        token,
        ActionMetadata::new(
            "add-project-path",
            "Project folder path",
            ActionRole::TextInput,
        ),
    );
    frames.record_bounds(
        token,
        "add-project-path".into(),
        Bounds::new(point(px(0.0), px(40.0)), size(px(280.0), px(44.0))),
    );
    // The frame is published only after every control -- including the
    // dialog's lazily-registered input -- has registered.
    frames.finalize_id(42);

    let order = frames.snapshot_focus_order_id(42);
    assert_eq!(order.get("add-project-browse"), Some(&0));
    assert_eq!(
        order.get("add-project-path"),
        Some(&1),
        "a dialog's lazily-registered control must get a contiguous focus \
         order once the frame is finalized, not the raw registration index \
         it happened to have when its bounds were recorded"
    );
}

#[test]
fn native_owner_reconciliation_updates_the_single_action_tree() {
    let mut tree = ActionTree::default();
    tree.registrar()
        .record(ActionMetadata::new("first", "First", ActionRole::Button));
    tree.registrar()
        .record(ActionMetadata::new("second", "Second", ActionRole::Button));
    tree.ensure_focus_owner();
    assert!(tree.set_focus_owner("second"));
    assert_eq!(
        tree.focusable()
            .find(|action| action.is_focused())
            .map(|action| action.id().as_ref()),
        Some("second")
    );
    assert!(!tree.set_focus_owner("missing"));
}

#[test]
fn indexed_action_lookup_keeps_ordered_focus_mutation_consistent() {
    let mut tree = ActionTree::default();
    tree.registrar()
        .record(ActionMetadata::new("first", "First", ActionRole::Button));
    tree.registrar()
        .record(ActionMetadata::new("second", "Second", ActionRole::Button));
    tree.registrar()
        .record(ActionMetadata::new("third", "Third", ActionRole::Button));

    assert!(!tree.registrar().record(ActionMetadata::new(
        "second",
        "Duplicate second",
        ActionRole::Button,
    )));
    assert_eq!(
        tree.iter()
            .map(|action| action.id().as_ref())
            .collect::<Vec<_>>(),
        ["first", "second", "third"]
    );
    assert_eq!(
        tree.get("second").map(|action| action.label().as_ref()),
        Some("Second")
    );
    assert!(tree.get("missing").is_none());
    assert!(tree.set_focus_owner("third"));
    assert_eq!(
        tree.focusable()
            .find(|action| action.is_focused())
            .map(|action| action.id().as_ref()),
        Some("third")
    );
    assert!(tree.set_pointer_state("first", ActionState::Hovered, true));
    assert!(tree.iter().next().is_some_and(|action| action.is_hovered()));
}

#[test]
fn pointer_states_stay_with_the_live_action_id() {
    let mut tree = ActionTree::default();
    tree.registrar()
        .record(ActionMetadata::new("open", "Open", ActionRole::Button));
    assert!(tree.set_pointer_state("open", ActionState::Hovered, true));
    assert!(tree.set_pointer_state("open", ActionState::Pressed, true));
    let action = tree.iter().next().expect("action");
    assert!(action.is_hovered());
    assert!(action.is_pressed());
    assert!(!tree.set_pointer_state("missing", ActionState::Hovered, true));
}

#[test]
fn overlay_registration_is_ordered_and_generation_scoped() {
    let frames = super::ActionFrames::default();
    let first = frames.begin_id(41, "package");
    frames.register(
        first,
        super::ActionMetadata::new("package-header", "Package", ActionRole::Navigation),
    );
    frames.register(
        first,
        super::ActionMetadata::new("source-overlay", "Source", ActionRole::Disclosure),
    );
    let visible = frames.snapshot_id(41);
    assert_eq!(
        visible
            .iter()
            .map(|action| action.id().as_ref())
            .collect::<Vec<_>>(),
        ["package-header", "source-overlay"]
    );

    let second = frames.begin_id(41, "package");
    frames.register(
        first,
        super::ActionMetadata::new("late-overlay", "Late", ActionRole::Disclosure),
    );
    frames.register(
        second,
        super::ActionMetadata::new("package-header", "Package", ActionRole::Navigation),
    );
    assert_eq!(
        frames
            .snapshot_id(41)
            .iter()
            .map(|action| action.id().as_ref())
            .collect::<Vec<_>>(),
        ["package-header"]
    );
}

#[test]
fn modal_finalization_traps_focus_and_keeps_restore_owner() {
    let mut tree = ActionTree::default();
    let mut registration = tree.registrar();
    assert!(registration.record(
        ActionMetadata::new("open-settings", "Settings", ActionRole::Button).focused(true)
    ));
    assert!(registration.record(ActionMetadata::new(
        "settings-dialog",
        "Settings",
        ActionRole::Dialog,
    )));
    assert!(registration.record(
        ActionMetadata::new("close-settings", "Done", ActionRole::Button).parent("settings-dialog")
    ));
    drop(registration);

    tree.finalize_modal();
    assert_eq!(
        tree.modal_root().map(|id| id.as_ref()),
        Some("settings-dialog")
    );
    assert!(tree.focus_trap());
    assert_eq!(
        tree.restore_focus().map(|id| id.as_ref()),
        Some("open-settings")
    );
    assert!(
        !tree
            .iter()
            .find(|action| action.id().as_ref() == "open-settings")
            .expect("restore owner")
            .is_focusable()
    );
    let focused = tree
        .iter()
        .find(|action| action.is_focused())
        .expect("dialog focus owner");
    assert_eq!(focused.id().as_ref(), "close-settings");
    assert_eq!(focused.focus_order(), 0);
}

#[test]
fn semantic_nodes_retain_every_rendered_component_state() {
    let mut tree = ActionTree::default();
    let mut registration = tree.registrar();
    assert!(
        registration.record(
            ActionMetadata::new("package", "serde", ActionRole::TreeItem)
                .enabled(true)
                .visible(true)
                .focused(true)
                .selected(true)
                .expanded(true)
                .loading(true)
                .error("index unavailable")
                .with_value("serde 1.0.0")
                .with_bounds(SemanticBounds::Logical {
                    x: 8,
                    y: 16,
                    width: 320,
                    height: 36,
                }),
        )
    );
    let node = tree.iter().next().expect("semantic node");
    assert!(node.is_enabled());
    assert!(node.is_visible());
    assert!(node.is_focused());
    assert!(node.is_selected());
    assert!(node.is_expanded());
    assert!(node.is_loading());
    assert_eq!(
        node.error_value().map(|value| value.as_ref()),
        Some("index unavailable")
    );
    assert_eq!(
        node.value().map(|value| value.as_ref()),
        Some("serde 1.0.0")
    );
    assert_eq!(
        node.bounds(),
        SemanticBounds::Logical {
            x: 8,
            y: 16,
            width: 320,
            height: 36,
        }
    );
}
