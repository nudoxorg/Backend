//! Persistent parser snapshots: appending never clones checked prefix ASTs or
//! flattens the whole prior source. Full source is materialized only on demand.

use super::node::BlockNode;
use gpui::SharedString;
use ropey::Rope;
use std::{
    ops::{Index, Range},
    sync::{Arc, OnceLock},
};

#[derive(Clone, Debug)]
pub(crate) struct SourceSnapshot {
    rope: Rope,
    flat: Arc<OnceLock<SharedString>>,
    #[cfg(test)]
    copy_work: Arc<std::sync::atomic::AtomicUsize>,
}

impl Default for SourceSnapshot {
    fn default() -> Self {
        Self::from(String::new())
    }
}
impl From<String> for SourceSnapshot {
    fn from(source: String) -> Self {
        Self {
            rope: Rope::from_str(&source),
            flat: Arc::default(),
            #[cfg(test)]
            copy_work: Arc::new(std::sync::atomic::AtomicUsize::new(source.len())),
        }
    }
}
impl From<&str> for SourceSnapshot {
    fn from(source: &str) -> Self {
        Self::from(source.to_owned())
    }
}
impl PartialEq for SourceSnapshot {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.flat, &other.flat) || self.rope == other.rope
    }
}
impl SourceSnapshot {
    pub(crate) fn len(&self) -> usize {
        self.rope.len_bytes()
    }
    pub(crate) fn append(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.rope.insert(self.rope.len_chars(), text);
        self.flat = Arc::default();
        #[cfg(test)]
        self.copy_work
            .fetch_add(text.len(), std::sync::atomic::Ordering::Relaxed);
    }
    pub(crate) fn get(&self, range: Range<usize>) -> Option<String> {
        if range.start > range.end || range.end > self.len() {
            return None;
        }
        let start = self.rope.byte_to_char(range.start);
        let end = self.rope.byte_to_char(range.end);
        if self.rope.char_to_byte(start) != range.start || self.rope.char_to_byte(end) != range.end
        {
            return None;
        }
        let result = self.rope.slice(start..end).to_string();
        #[cfg(test)]
        self.copy_work
            .fetch_add(result.len(), std::sync::atomic::Ordering::Relaxed);
        Some(result)
    }
    #[cfg(test)]
    pub(crate) fn record_materialized_bytes(&self, bytes: usize) {
        self.copy_work
            .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn as_str(&self) -> &str {
        self.flat
            .get_or_init(|| {
                #[cfg(test)]
                self.copy_work
                    .fetch_add(self.len(), std::sync::atomic::Ordering::Relaxed);
                self.rope.to_string().into()
            })
            .as_str()
    }
    pub(crate) fn shared(&self) -> SharedString {
        self.as_str();
        self.flat.get().unwrap().clone()
    }
    #[cfg(test)]
    pub(crate) fn copied_bytes(&self) -> usize {
        self.copy_work.load(std::sync::atomic::Ordering::Relaxed)
    }
}
impl std::fmt::Display for SourceSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug)]
enum Tree {
    Leaf(BlockNode),
    Branch {
        left: Arc<Tree>,
        right: Arc<Tree>,
        len: usize,
        height: usize,
    },
}
impl Tree {
    fn len(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Branch { len, .. } => *len,
        }
    }
    fn height(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Branch { height, .. } => *height,
        }
    }
    fn get(&self, index: usize) -> Option<&BlockNode> {
        match self {
            Self::Leaf(block) => (index == 0).then_some(block),
            Self::Branch { left, right, .. } => {
                if index < left.len() {
                    left.get(index)
                } else {
                    right.get(index - left.len())
                }
            }
        }
    }
}
fn branch(left: Arc<Tree>, right: Arc<Tree>) -> Arc<Tree> {
    Arc::new(Tree::Branch {
        len: left.len() + right.len(),
        height: left.height().max(right.height()) + 1,
        left,
        right,
    })
}
fn balance(left: Arc<Tree>, right: Arc<Tree>) -> Arc<Tree> {
    if left.height() > right.height() + 1 {
        let Tree::Branch {
            left: ll,
            right: lr,
            ..
        } = left.as_ref()
        else {
            unreachable!()
        };
        if ll.height() >= lr.height() {
            return branch(ll.clone(), branch(lr.clone(), right));
        }
        let Tree::Branch {
            left: lrl,
            right: lrr,
            ..
        } = lr.as_ref()
        else {
            unreachable!()
        };
        return branch(branch(ll.clone(), lrl.clone()), branch(lrr.clone(), right));
    }
    if right.height() > left.height() + 1 {
        let Tree::Branch {
            left: rl,
            right: rr,
            ..
        } = right.as_ref()
        else {
            unreachable!()
        };
        if rr.height() >= rl.height() {
            return branch(branch(left, rl.clone()), rr.clone());
        }
        let Tree::Branch {
            left: rll,
            right: rlr,
            ..
        } = rl.as_ref()
        else {
            unreachable!()
        };
        return branch(branch(left, rll.clone()), branch(rlr.clone(), rr.clone()));
    }
    branch(left, right)
}
fn join(left: Arc<Tree>, right: Arc<Tree>) -> Arc<Tree> {
    if left.height() > right.height() + 1 {
        let Tree::Branch {
            left: ll,
            right: lr,
            ..
        } = left.as_ref()
        else {
            unreachable!()
        };
        return balance(ll.clone(), join(lr.clone(), right));
    }
    if right.height() > left.height() + 1 {
        let Tree::Branch {
            left: rl,
            right: rr,
            ..
        } = right.as_ref()
        else {
            unreachable!()
        };
        return balance(join(left, rl.clone()), rr.clone());
    }
    branch(left, right)
}
fn remove_last(root: &Arc<Tree>) -> Option<Arc<Tree>> {
    match root.as_ref() {
        Tree::Leaf(_) => None,
        Tree::Branch { left, right, .. } => Some(match remove_last(right) {
            Some(right) => join(left.clone(), right),
            None => left.clone(),
        }),
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BlockSequence(Option<Arc<Tree>>);
impl From<Vec<BlockNode>> for BlockSequence {
    fn from(blocks: Vec<BlockNode>) -> Self {
        fn build(mut blocks: std::vec::IntoIter<BlockNode>, count: usize) -> Option<Arc<Tree>> {
            if count == 0 {
                return None;
            }
            // Build by balanced concatenation; leaves move in, never clone.
            let mut root = Arc::new(Tree::Leaf(blocks.next().unwrap()));
            for block in blocks {
                root = join(root, Arc::new(Tree::Leaf(block)));
            }
            Some(root)
        }
        let count = blocks.len();
        Self(build(blocks.into_iter(), count))
    }
}
impl PartialEq for BlockSequence {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) if Arc::ptr_eq(a, b) => true,
            _ => self.len() == other.len() && self.iter().eq(other.iter()),
        }
    }
}
impl BlockSequence {
    pub(crate) fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |root| root.len())
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_none()
    }
    pub(crate) fn get(&self, index: usize) -> Option<&BlockNode> {
        self.0.as_ref()?.get(index)
    }
    pub(crate) fn last(&self) -> Option<&BlockNode> {
        self.get(self.len().checked_sub(1)?)
    }
    pub(crate) fn pop(&mut self) {
        self.0 = self.0.as_ref().and_then(remove_last);
    }
    pub(crate) fn append(&mut self, next: Self) {
        self.0 = match (self.0.take(), next.0) {
            (Some(left), Some(right)) => Some(join(left, right)),
            (None, right) => right,
            (left, None) => left,
        };
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &BlockNode> {
        (0..self.len()).map(move |index| &self[index])
    }
    pub(crate) fn into_vec(self) -> Vec<BlockNode> {
        self.iter().cloned().collect()
    }
}
impl Index<usize> for BlockSequence {
    type Output = BlockNode;
    fn index(&self, index: usize) -> &BlockNode {
        self.get(index).expect("block index out of bounds")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_blocks_keep_prefix_identity_and_balanced_paths() {
        let first: BlockSequence = vec![BlockNode::HorizontalRule { span: None }].into();
        let first_address = first.get(0).unwrap() as *const BlockNode;
        let mut grown = first.clone();
        for _ in 0..4096 {
            grown.append(vec![BlockNode::HorizontalRule { span: None }].into());
        }
        assert_eq!(grown.get(0).unwrap() as *const BlockNode, first_address);
        assert_eq!(first.len(), 1);
        assert_eq!(grown.len(), 4097);
        assert!(grown.0.as_ref().unwrap().height() <= 26);
        for _ in 0..4096 {
            grown.pop();
        }
        assert_eq!(grown.get(0).unwrap() as *const BlockNode, first_address);
        assert_eq!(grown.len(), 1);
    }

    #[test]
    fn source_snapshot_copies_appended_bytes_without_flattening_prefix() {
        let original = SourceSnapshot::from("🦀 initial\n\n".to_owned());
        let mut grown = original.clone();
        let before = grown.copied_bytes();
        for _ in 0..4096 {
            grown.append("new paragraph\n\n");
        }
        assert_eq!(
            grown.copied_bytes() - before,
            4096 * "new paragraph\n\n".len()
        );
        assert_eq!(original.len(), "🦀 initial\n\n".len());
        assert!(grown.flat.get().is_none());
        assert_eq!(grown.get(0..4).unwrap(), "🦀");
        assert!(grown.get(1..4).is_none());
    }
}
