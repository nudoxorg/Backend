//! Small traversal helpers over shared persistent tree handles.

use super::node::Node;

pub(crate) fn leaves(root: &Node) -> Vec<Node> {
    let mut output = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        if node.entries().is_some() {
            output.push(node);
            continue;
        }
        let children = node.children().collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
    output
}
