// Copyright 2026 The Nudox contributors.
// Licensed under the Apache License, Version 2.0 or the MIT license, at your option.

use accesskit::{
    Action, ActionHandler, ActionRequest, Node, NodeId as LocalNodeId, Rect, Role, Tree, TreeId,
    TreeUpdate,
};
use atspi_common::Interface;
use std::sync::{Arc, Mutex};

use crate::{
    Adapter, AdapterCallback, AppContext, Event, InterfaceSet, NodeId, NodeIdOrRoot, ObjectEvent,
    PlatformNode, State, WindowBounds,
};

#[derive(Clone, Default)]
struct Callback {
    states: Arc<Mutex<Vec<(NodeId, State, bool)>>>,
}

impl AdapterCallback for Callback {
    fn register_interfaces(&self, _: &Adapter, _: NodeId, _: InterfaceSet) {}
    fn unregister_interfaces(&self, _: &Adapter, _: NodeId, _: InterfaceSet) {}

    fn emit_event(&self, _: &Adapter, event: Event) {
        if let Event::Object {
            target: NodeIdOrRoot::Node(id),
            event: ObjectEvent::StateChanged(state, value),
        } = event
        {
            self.states.lock().unwrap().push((id, state, value));
        }
    }
}

struct NoOpActionHandler;

impl ActionHandler for NoOpActionHandler {
    fn do_action(&mut self, _: ActionRequest) {}
}

fn build(node: Node, focused: bool) -> (Adapter, PlatformNode, Callback) {
    let mut window = Node::new(Role::Window);
    window.set_children(vec![LocalNodeId(1)]);
    let initial = TreeUpdate {
        nodes: vec![(LocalNodeId(0), window), (LocalNodeId(1), node)],
        tree: Some(Tree::new(LocalNodeId(0))),
        tree_id: TreeId::ROOT,
        focus: LocalNodeId(if focused { 1 } else { 0 }),
    };
    let callback = Callback::default();
    let adapter = Adapter::new(
        &AppContext::new(None),
        callback.clone(),
        initial,
        true,
        WindowBounds::default(),
        NoOpActionHandler,
    );
    let child_id = adapter
        .platform_node(adapter.root_id())
        .child_at_index(0)
        .unwrap()
        .unwrap();
    let child = adapter.platform_node(child_id);
    (adapter, child, callback)
}

fn button(disabled: bool, click: bool) -> Node {
    let mut node = Node::new(Role::Button);
    node.set_bounds(Rect::new(0.0, 0.0, 80.0, 30.0));
    if disabled {
        node.set_disabled();
    } else {
        node.add_action(Action::Focus);
    }
    if click {
        node.add_action(Action::Click);
    }
    node
}

#[test]
fn disabled_button_state_is_independent_of_click_interface() {
    for disabled in [false, true] {
        for click in [false, true] {
            let (_adapter, child, _) = build(button(disabled, click), !disabled);
            let state = child.state();
            assert_eq!(state.contains(State::Enabled), !disabled);
            assert_eq!(state.contains(State::Sensitive), !disabled);
            assert!(!state.contains(State::ReadOnly));
            assert_eq!(state.contains(State::Focusable), !disabled);
            assert_eq!(state.contains(State::Focused), !disabled);
            assert!(state.contains(State::Visible | State::Showing));

            // An absent Click action is valid for both enabled and disabled buttons.
            // Conversely, explicit provider actions retain their existing behavior.
            let interfaces = child.interfaces().unwrap();
            assert!(interfaces.contains(Interface::Accessible));
            assert!(interfaces.contains(Interface::Component));
            assert_eq!(interfaces.contains(Interface::Action), click);
            assert_eq!(child.supports_action().unwrap(), click);
            assert_eq!(child.n_actions().unwrap(), i32::from(click));
        }
    }
}

#[test]
fn text_inputs_preserve_enabled_read_only_and_disabled_states() {
    for role in [Role::TextInput, Role::MultilineTextInput] {
        for (read_only, disabled) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut node = Node::new(role);
            node.add_action(Action::Focus);
            if read_only {
                node.set_read_only();
            }
            if disabled {
                node.set_disabled();
            }
            let (_adapter, child, _) = build(node, true);
            let state = child.state();
            let enabled = !read_only && !disabled;
            assert_eq!(state.contains(State::Enabled), enabled);
            assert_eq!(state.contains(State::Sensitive), enabled);
            assert_eq!(state.contains(State::ReadOnly), read_only || disabled);
            // Preserve upstream Editable semantics, including disabled text inputs.
            assert_eq!(state.contains(State::Editable), !read_only);
            assert!(state.contains(State::Focusable | State::Focused));
            assert!(state.contains(State::SelectableText));
            assert_eq!(
                state.contains(State::MultiLine),
                role == Role::MultilineTextInput
            );
            assert_eq!(state.contains(State::SingleLine), role == Role::TextInput);
            assert!(!child.interfaces().unwrap().contains(Interface::Action));
        }
    }
}

#[test]
fn disabling_and_reenabling_button_emits_enabled_and_sensitive_changes() {
    let (mut adapter, child, callback) = build(button(false, false), false);
    callback.states.lock().unwrap().clear();

    for disabled in [true, false] {
        adapter.update(TreeUpdate {
            nodes: vec![(LocalNodeId(1), button(disabled, false))],
            tree: None,
            tree_id: TreeId::ROOT,
            focus: LocalNodeId(0),
        });
        let changes = callback.states.lock().unwrap();
        for state in [State::Enabled, State::Sensitive] {
            assert!(changes.contains(&(child.id(), state, !disabled)));
            assert_eq!(child.state().contains(state), !disabled);
        }
        assert!(
            !changes
                .iter()
                .any(|(_, state, _)| *state == State::ReadOnly)
        );
        assert!(!child.interfaces().unwrap().contains(Interface::Action));
        drop(changes);
        callback.states.lock().unwrap().clear();
    }
}
