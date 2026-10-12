//! Count-only adaptation of OpenAI tiktoken 0.11.0's Rust `encode_ordinary`
//! and rank-ordered byte-pair merging (MIT; token_data/LICENSE).
//!
//! The encoding is explicitly cl100k_base ordinary text, not a claim about
//! every model. Special-looking text is ordinary user data. The immutable
//! vocabulary is loaded once. Input, regex backtracking and merge scratch are
//! bounded; a heap replaces the upstream quadratic minimum-rank rescan while
//! preserving its rank/leftmost tie order. No token strings are allocated.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::OnceLock;

/// Exact encoding policy used for MCP admission.
pub const CL100K_POLICY: &str = "cl100k_base/ordinary-v1";
/// SHA-256 of the retained rank-ordered vocabulary asset.
pub const CL100K_VOCABULARY_SHA256: &str =
    "69111b35d5b2fed760d611fd634c3dd0f75310ccfc1938740cda69041674e992";
const VOCABULARY: &[u8] = include_bytes!("token_data/cl100k.ranks");
const PATTERN: &str = r"'(?i:[sdmt]|ll|ve|re)|[^\r\n\p{L}\p{N}]?+\p{L}++|\p{N}{1,3}+| ?[^\s\p{L}\p{N}]++[\r\n]*+|\s++$|\s*[\r\n]|\s+(?!\S)|\s";
const MAX_PIECE: usize = 128;

/// Closed refusal from the bounded named-tokenizer admission boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cl100kError {
    /// Input exceeded the shared hard payload bound before scratch allocation.
    InputTooLarge,
    /// The compiled-in vocabulary or regex could not be admitted.
    InvalidEncoding,
    /// Regex matching exhausted its finite budget or did not partition input.
    PatternRefused,
}
impl std::fmt::Display for Cl100kError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InputTooLarge => "cl100k input exceeds the hard payload bound",
            Self::InvalidEncoding => "cl100k retained encoding could not be admitted",
            Self::PatternRefused => "cl100k pattern matching exceeded its bounded work",
        })
    }
}
struct Counter {
    ranks: HashMap<&'static [u8], u32>,
    pattern: fancy_regex::Regex,
}
impl Counter {
    fn load() -> Result<Self, Cl100kError> {
        if VOCABULARY.get(..8) != Some(b"CL100K\x01\x00") {
            return Err(Cl100kError::InvalidEncoding);
        }
        let mut offset = 12;
        if VOCABULARY.get(8..12) != Some(&100_256_u32.to_le_bytes()) {
            return Err(Cl100kError::InvalidEncoding);
        }
        let mut ranks = HashMap::with_capacity(100_256);
        for rank in 0..100_256_u32 {
            let length = VOCABULARY
                .get(offset..offset + 2)
                .ok_or(Cl100kError::InvalidEncoding)?;
            let length = usize::from(u16::from_le_bytes([length[0], length[1]]));
            offset += 2;
            if !(1..=MAX_PIECE).contains(&length) {
                return Err(Cl100kError::InvalidEncoding);
            }
            let token = VOCABULARY
                .get(offset..offset + length)
                .ok_or(Cl100kError::InvalidEncoding)?;
            offset += length;
            if ranks.insert(token, rank).is_some() {
                return Err(Cl100kError::InvalidEncoding);
            }
        }
        if offset != VOCABULARY.len() || !(0..=255_u8).all(|byte| ranks.contains_key(&[byte][..])) {
            return Err(Cl100kError::InvalidEncoding);
        }
        let pattern = fancy_regex::RegexBuilder::new(PATTERN)
            .backtrack_limit(1_000_000)
            .build()
            .map_err(|_| Cl100kError::InvalidEncoding)?;
        Ok(Self { ranks, pattern })
    }
    fn count(&self, text: &str) -> Result<usize, Cl100kError> {
        let mut count = 0;
        let mut end = 0;
        for found in self.pattern.find_iter(text) {
            let found = found.map_err(|_| Cl100kError::PatternRefused)?;
            if found.start() != end || found.end() <= end {
                return Err(Cl100kError::PatternRefused);
            }
            end = found.end();
            let piece = found.as_str().as_bytes();
            count += if self.ranks.contains_key(piece) {
                1
            } else {
                self.merge_count(piece)
            };
        }
        if end != text.len() {
            return Err(Cl100kError::PatternRefused);
        }
        Ok(count)
    }
    fn merge_count(&self, piece: &[u8]) -> usize {
        // Nodes retain original byte offsets. Rank ties use the left byte
        // offset, exactly the upstream first-minimum scan. A stale candidate
        // never merges a changed or nonadjacent pair.
        let mut nodes: Vec<_> = (0..piece.len())
            .map(|start| Part {
                end: start + 1,
                previous: start.checked_sub(1),
                next: (start + 1 < piece.len()).then_some(start + 1),
                generation: 0,
                live: true,
            })
            .collect();
        let mut heap = BinaryHeap::new();
        for left in 0..piece.len().saturating_sub(1) {
            self.push_pair(piece, &nodes, left, &mut heap);
        }
        let mut count = piece.len();
        while let Some(Reverse((_rank, left, left_generation, right, right_generation))) =
            heap.pop()
        {
            if !nodes[left].live
                || !nodes[right].live
                || nodes[left].generation != left_generation
                || nodes[right].generation != right_generation
                || nodes[left].next != Some(right)
            {
                continue;
            }
            let next = nodes[right].next;
            nodes[left].end = nodes[right].end;
            nodes[left].next = next;
            nodes[left].generation += 1;
            nodes[right].live = false;
            if let Some(next) = next {
                nodes[next].previous = Some(left);
            }
            count -= 1;
            if let Some(previous) = nodes[left].previous {
                self.push_pair(piece, &nodes, previous, &mut heap);
            }
            self.push_pair(piece, &nodes, left, &mut heap);
        }
        count
    }
    fn push_pair(
        &self,
        piece: &[u8],
        nodes: &[Part],
        left: usize,
        heap: &mut BinaryHeap<Reverse<Pair>>,
    ) {
        if let Some(right) = nodes[left].next {
            let end = nodes[right].end;
            if end - left <= MAX_PIECE {
                if let Some(rank) = self.ranks.get(&piece[left..end]) {
                    heap.push(Reverse((
                        *rank,
                        left,
                        nodes[left].generation,
                        right,
                        nodes[right].generation,
                    )));
                }
            }
        }
    }
}
struct Part {
    end: usize,
    previous: Option<usize>,
    next: Option<usize>,
    generation: usize,
    live: bool,
}
type Pair = (u32, usize, usize, usize, usize);

/// Counts ordinary cl100k_base tokens for a bounded UTF-8 payload.
/// Initialization is shared; scratch is caller-owned and released on return.
///
/// # Errors
/// Refuses excessive input, invalid retained data, or bounded regex failure.
pub fn cl100k_tokens(text: &str) -> Result<usize, Cl100kError> {
    if text.len() > crate::MAX_RESPONSE_BUDGET_BYTES {
        return Err(Cl100kError::InputTooLarge);
    }
    static COUNTER: OnceLock<Result<Counter, Cl100kError>> = OnceLock::new();
    COUNTER
        .get_or_init(Counter::load)
        .as_ref()
        .map_err(|error| *error)?
        .count(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_reference_matches_ordinary_tokens_for_unicode_escaping_and_long_punctuation() {
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("token_data/reference.json"))
                .expect("retained oracle");
        for case in reference["cases"].as_array().expect("cases") {
            let text = case["text"].as_str().expect("text");
            assert_eq!(
                cl100k_tokens(text).expect("bounded reference"),
                case["tokens"].as_u64().expect("count") as usize,
                "{text:?}"
            );
        }
    }
    // Literal reference minimum-rank/leftmost semantics from tiktoken's Rust
    // byte_pair_encode. Independent vector scan checks the heap's stale links.
    fn reference_merge(counter: &Counter, bytes: &[u8]) -> usize {
        let mut ranges: Vec<_> = (0..bytes.len()).map(|i| (i, i + 1)).collect();
        loop {
            let next = ranges
                .windows(2)
                .enumerate()
                .filter_map(|(i, pair)| {
                    counter
                        .ranks
                        .get(&bytes[pair[0].0..pair[1].1])
                        .map(|rank| (*rank, i))
                })
                .min();
            let Some((_, i)) = next else {
                return ranges.len();
            };
            ranges[i].1 = ranges[i + 1].1;
            ranges.remove(i + 1);
        }
    }
    #[test]
    fn heap_merge_obeys_reference_rank_ties_and_adjacent_update_laws() {
        let counter = Counter::load().expect("retained encoding");
        let mut state = 0xace1_u32;
        for width in 1..300 {
            let bytes: Vec<_> = (0..width)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    (state % 95 + 32) as u8
                })
                .collect();
            assert_eq!(
                counter.merge_count(&bytes),
                reference_merge(&counter, &bytes)
            );
        }
    }
    #[test]
    fn tokenizer_refuses_excess_input_before_merge_scratch() {
        assert_eq!(
            cl100k_tokens(&"!".repeat(crate::MAX_RESPONSE_BUDGET_BYTES + 1)),
            Err(Cl100kError::InputTooLarge)
        );
        assert_eq!(
            cl100k_tokens("<|endoftext|>").expect("ordinary special spelling"),
            7
        );
    }
}
