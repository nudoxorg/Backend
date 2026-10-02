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
    /// Test-only byte-copy volume for rope construction, materialized ranges,
    /// appends, and explicit whole-source flattening.
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
        self.rope.len()
    }

    /// Check a suffix without materializing the whole rope or allocating a
    /// temporary string. One trailing line ending still permits a Markdown
    /// reference definition to take a continuation title on the next line;
    /// a blank line closes that grammar boundary.
    pub(crate) fn reference_definition_can_continue(&self, end_offset: usize) -> bool {
        if end_offset > self.len() {
            return false;
        }
        let start = self.rope.byte_to_char_idx(end_offset);
        if self.rope.char_to_byte_idx(start) != end_offset {
            return false;
        }

        let mut line_endings = 0;
        let mut previous_was_cr = false;
        for character in self.rope.slice(start..self.rope.len_chars()).chars() {
            match character {
                '\r' => {
                    line_endings += 1;
                    previous_was_cr = true;
                }
                '\n' => {
                    if !previous_was_cr {
                        line_endings += 1;
                    }
                    previous_was_cr = false;
                }
                ' ' | '\t' => previous_was_cr = false,
                _ => return false,
            }
            if line_endings > 1 {
                return false;
            }
        }
        true
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
        let start = self.rope.byte_to_char_idx(range.start);
        let end = self.rope.byte_to_char_idx(range.end);
        if self.rope.char_to_byte_idx(start) != range.start
            || self.rope.char_to_byte_idx(end) != range.end
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

#[derive(Clone, Debug)]
struct StoredBlock(Arc<BlockNode>);
#[derive(Clone, Debug, Default)]
struct BlockCount(usize);
impl sum_tree::ContextLessSummary for BlockCount {
    fn zero() -> Self {
        Self(0)
    }
    fn add_summary(&mut self, other: &Self) {
        self.0 += other.0;
    }
}
impl sum_tree::Item for StoredBlock {
    type Summary = BlockCount;
    fn summary(&self, (): ()) -> BlockCount {
        BlockCount(1)
    }
}
impl<'a> sum_tree::Dimension<'a, BlockCount> for usize {
    fn zero((): ()) -> Self {
        0
    }
    fn add_summary(&mut self, summary: &'a BlockCount, (): ()) {
        *self += summary.0;
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BlockSequence(sum_tree::SumTree<StoredBlock>);
impl From<Vec<BlockNode>> for BlockSequence {
    fn from(blocks: Vec<BlockNode>) -> Self {
        Self(sum_tree::SumTree::from_iter(
            blocks.into_iter().map(|block| StoredBlock(Arc::new(block))),
            (),
        ))
    }
}
impl PartialEq for BlockSequence {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}
impl BlockSequence {
    pub(crate) fn len(&self) -> usize {
        self.0.summary().0
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn get(&self, index: usize) -> Option<&BlockNode> {
        if index >= self.len() {
            return None;
        }
        let mut cursor = self.0.cursor::<usize>(());
        cursor.seek(&index, sum_tree::Bias::Right);
        cursor.item().map(|block| block.0.as_ref())
    }
    pub(crate) fn last(&self) -> Option<&BlockNode> {
        self.0.last().map(|block| block.0.as_ref())
    }
    pub(crate) fn pop(&mut self) {
        if self.is_empty() {
            return;
        }
        let prefix = {
            let mut cursor = self.0.cursor::<usize>(());
            cursor.slice(&(self.len() - 1), sum_tree::Bias::Left)
        };
        self.0 = prefix;
    }
    pub(crate) fn append(&mut self, next: Self) {
        self.0.append(next.0, ());
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &BlockNode> {
        self.0.iter().map(|block| block.0.as_ref())
    }
    pub(crate) fn iter_from(&self, index: usize) -> impl Iterator<Item = (usize, &BlockNode)> {
        let mut cursor = self.0.cursor::<usize>(());
        cursor.seek(&index, sum_tree::Bias::Right);
        cursor
            .enumerate()
            .map(move |(offset, block)| (index + offset, block.0.as_ref()))
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
        assert_eq!(grown.iter().count(), 4097);
        assert_eq!(
            grown
                .iter_from(4094)
                .take(3)
                .map(|(index, _)| index)
                .collect::<Vec<_>>(),
            [4094, 4095, 4096]
        );
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

    #[test]
    fn reference_tail_probe_is_utf8_safe_and_stops_at_a_blank_line() {
        let source = SourceSnapshot::from("🦀 [id]: fo".to_owned());
        let definition_end = "🦀 [id]: fo".len();
        assert!(source.reference_definition_can_continue(definition_end));

        let mut one_line = source.clone();
        one_line.append(" \r\n");
        assert!(one_line.reference_definition_can_continue(definition_end));

        let mut closed = one_line.clone();
        closed.append("\r\n");
        assert!(!closed.reference_definition_can_continue(definition_end));
        assert!(!source.reference_definition_can_continue(1));
    }
}
