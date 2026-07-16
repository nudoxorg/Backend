//! Keyword-normalization heuristics — ported from lib.rs `categories/synonyms.rs`.
//!
//! Three public items:
//! - [`normalize_keyword`] — pure canonicalizer (no data files needed).
//! - [`Synonyms`] — loads `tag-synonyms.csv` and maps near-duplicate keywords to
//!   canonical forms with a vote-weighted relevance score.
//! - [`Specifics`] — loads `specific-keywords.txt` and `bland-keywords.txt` and
//!   exposes a DSL for keyword relations (specificity, combination, splitting, and
//!   blandness).

use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::{fmt, fs, io};

use smol_str::SmolStr;

// ===========================================================================
// normalize_keyword
// ===========================================================================

/// Canonicalize a raw keyword string.
#[must_use]
pub fn normalize_keyword(input_keyword: &str) -> SmolStr {
    if is_already_canonical(input_keyword) {
        return input_keyword.trim_matches('-').into();
    }

    let tokens: Vec<Cow<str>> = input_keyword
        .split(|character: char| {
            character.is_ascii_whitespace() || character == '_' || character == '-'
        })
        .map(apply_well_known_replacements)
        .flat_map(split_on_slashes)
        .map(apply_formatting_rules)
        .filter(|token| !token.is_empty())
        .take(30)
        .collect();

    let joined_tokens = tokens.join(" ");
    let kebab_cased = inline_kebab_case(&joined_tokens);

    truncate_keyword(&kebab_cased, 55, 65)
}

#[inline]
fn is_already_canonical(input: &str) -> bool {
    input
        .as_bytes()
        .iter()
        .all(|&byte| (byte.is_ascii_lowercase() && byte.is_ascii_alphanumeric()) || byte == b'-')
}

fn apply_well_known_replacements(token: &str) -> Cow<str> {
    let stripped = token.trim_end_matches("'s").trim_end_matches("\u{2019}s");
    let replacement = match stripped {
        "I/O" | "i/o" => "io",
        "C++" | "c++" => "cpp",
        "C/C++" => "c-or-cpp",
        "OSes" => "os",
        "LaTeX" => "latex",
        "XeTeX" => "xetex",
        "CHAdeMO" => "chademo",
        other => other,
    };
    Cow::Borrowed(replacement)
}

fn split_on_slashes(token: Cow<str>) -> Vec<String> {
    token.into_owned().split('/').map(String::from).collect()
}

fn apply_formatting_rules(token: String) -> Cow<'static, str> {
    let mut token = Cow::Owned(token);

    token = strip_plural_acronyms(token);
    token = replace_symbols(token);
    token = normalize_byte_suffixes(token);
    token = normalize_mixed_casing(token);
    token = format_standard_prefixes(token);
    token = lowercase_script_suffix(token);

    token
}

fn strip_plural_acronyms(token: Cow<str>) -> Cow<str> {
    let bytes = token.as_bytes();
    let length = bytes.len();

    if length > 2 && bytes[length - 1] == b's' && bytes[length - 2].is_ascii_uppercase() {
        return Cow::Owned(token[..length - 1].to_owned());
    }
    token
}

fn replace_symbols(mut token: Cow<str>) -> Cow<str> {
    if token.contains('%') {
        token = Cow::Owned(token.replace('%', "percent"));
    }
    if token.contains('&') {
        token = Cow::Owned(token.replace('&', " and "));
    }
    token
}

fn normalize_byte_suffixes(token: Cow<str>) -> Cow<str> {
    if token.ends_with("iB") {
        return Cow::Owned(token.to_ascii_lowercase());
    }
    token
}

fn normalize_mixed_casing(token: Cow<str>) -> Cow<str> {
    let mut characters = token.chars();
    if let (Some(first), Some(second)) = (characters.next(), characters.next()) {
        let is_ios_style = first.is_ascii_lowercase() && second.is_ascii_uppercase();
        let is_tex_style = token.len() == 3
            && first.is_ascii_uppercase()
            && second.is_ascii_lowercase()
            && characters
                .next()
                .is_some_and(|char| char.is_ascii_uppercase());

        if is_ios_style || is_tex_style {
            return Cow::Owned(token.to_ascii_lowercase());
        }
    }
    token
}

fn format_standard_prefixes(token: Cow<str>) -> Cow<str> {
    let token_lower = token.to_ascii_lowercase();
    for prefix in ["rfc", "iso", "iec", "bcp"] {
        if let Some(rest) = token_lower.strip_prefix(prefix) {
            if rest.len() >= 2 && rest.bytes().all(|byte| byte.is_ascii_digit()) {
                return Cow::Owned(format!("{prefix}-{rest}"));
            }
        }
    }
    token
}

fn lowercase_script_suffix(token: Cow<str>) -> Cow<str> {
    if token.contains("Script") {
        return Cow::Owned(token.to_ascii_lowercase());
    }
    token
}

fn truncate_keyword(keyword: &str, target_length: usize, max_length: usize) -> SmolStr {
    let mut result = SmolStr::from(keyword.trim_matches('-'));

    if result.len() > max_length {
        if let Some(truncated) = result.get(..target_length) {
            result = SmolStr::from(format!("{truncated}…"));
        }
    }
    result
}

fn inline_kebab_case(input: &str) -> String {
    heck::AsKebabCase(input).to_string()
}

// ===========================================================================
// Synonyms
// ===========================================================================

pub struct Synonyms {
    mapping: HashMap<SmolStr, (SmolStr, u8)>,
}

impl Synonyms {
    #[cold]
    pub fn new(data_dir: &Path) -> io::Result<Self> {
        let path = data_dir.join("tag-synonyms.csv");
        if !path.exists() {
            tracing::warn!("tag-synonyms.csv not found at {:?}; using empty map", path);
            return Ok(Self {
                mapping: HashMap::new(),
            });
        }

        let mut reader = csv::ReaderBuilder::new()
            .comment(Some(b'#'))
            .has_headers(false)
            .trim(csv::Trim::All)
            .flexible(true)
            .from_path(&path)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let mut mapping = HashMap::with_capacity(2500);
        let mut needs_fixing = false;

        for result in reader.records() {
            let record = result.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if record.is_empty() {
                continue;
            }
            let find_keyword = SmolStr::from(record.get(0).unwrap_or(""));
            let replace_keyword = SmolStr::from(record.get(1).unwrap_or(""));
            let score: u8 = record
                .get(2)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData,
                    format!("Missing or invalid score in record: {:?}", record)))?;

            if score > 5 {
                tracing::error!("synonym borked score: {record:?}");
            }

            match mapping.entry(find_keyword) {
                Entry::Occupied(mut entry) => {
                    if entry.get().1 < score {
                        tracing::error!("duplicate synonym {record:?} and {}", entry.get().0);
                        entry.insert((replace_keyword, score));
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert((replace_keyword, score));
                }
            }
        }

        needs_fixing |= Self::remove_cyclic_synonyms(&mut mapping);

        if needs_fixing {
            tracing::warn!("synonym table had issues; consider regenerating tag-synonyms.csv");
        }

        Ok(Self { mapping })
    }

    fn remove_cyclic_synonyms(mapping: &mut HashMap<SmolStr, (SmolStr, u8)>) -> bool {
        let mut found_cycles = false;
        let mut to_remove = Vec::new();

        for key in mapping.keys() {
            let mut current = key.as_str();
            let chain_length = std::iter::from_fn(|| {
                current = mapping.get(current)?.0.as_str();
                Some(current)
            })
            .take(21)
            .count();

            if chain_length >= 20 {
                tracing::error!("synonym loop at {},{}", key, current);
                found_cycles = true;
                to_remove.push(key.clone());
            }
        }

        for key in to_remove {
            mapping.remove(&key);
        }

        found_cycles
    }

    #[inline]
    #[must_use]
    pub fn get(&self, keyword: &str) -> Option<(&str, f32)> {
        let (tag, votes) = self.mapping.get(keyword)?;
        let relevance = (f32::from(*votes) / 5.0 + 0.1).min(1.0);
        Some((tag.as_str(), relevance))
    }

    fn get_matching(&self, keyword: &str, min_votes: u8) -> Option<(&str, f32)> {
        let (tag, votes) = self.mapping.get(keyword)?;
        if *votes >= min_votes {
            return Some((tag.as_str(), f32::from(*votes) / 5.0));
        }
        None
    }

    #[must_use]
    pub fn max_normalize<'a>(&'a self, keyword: &'a str) -> Cow<'a, str> {
        self.max_normalize_inner(keyword, 2)
    }

    fn max_normalize_inner<'a>(&'a self, keyword: &'a str, depth: u8) -> Cow<'a, str> {
        let mut current_keyword: Cow<str> = Cow::Borrowed(
            self.mapping
                .get(keyword)
                .map(|(first_hop, _)| {
                    self.mapping
                        .get(first_hop.as_str())
                        .map(|(second_hop, _)| second_hop.as_str())
                        .unwrap_or(first_hop.as_str())
                })
                .unwrap_or(keyword),
        );

        if depth == 0 {
            return current_keyword;
        }

        let next_depth = depth - 1;
        let mut has_multiple_hyphens = false;

        if let Some((start, end)) = current_keyword.split_once('-') {
            has_multiple_hyphens = end.contains('-');

            let normalized_start = self.max_normalize_inner(start, next_depth);
            let normalized_end = self.max_normalize_inner(end, next_depth);

            if normalized_start != start || normalized_end != end {
                current_keyword = if normalized_start != normalized_end {
                    format!("{normalized_start}-{normalized_end}").into()
                } else {
                    normalized_start.to_string().into()
                };
            }
        }

        if has_multiple_hyphens {
            if let Some((start, end)) = current_keyword.rsplit_once('-') {
                let normalized_start = self.max_normalize_inner(start, next_depth);
                let normalized_end = self.max_normalize_inner(end, next_depth);

                if normalized_start != start || normalized_end != end {
                    current_keyword = format!("{normalized_start}-{normalized_end}").into();
                }
            }
        }

        current_keyword
    }

    #[must_use]
    pub fn normalize<'a>(&'a self, keyword: &'a str, min_votes: u8) -> (&'a str, f32) {
        debug_assert!(min_votes > 0 && min_votes <= 5);
        if let Some((first_hop, weight1)) = self.get_matching(keyword, min_votes.min(5)) {
            if let Some((second_hop, weight2)) =
                self.get_matching(first_hop, (min_votes + 1).clamp(4, 5))
            {
                return (second_hop, weight1 * weight2);
            }
            return (first_hop, weight1);
        }
        (keyword, 1.0)
    }

    #[inline]
    #[must_use]
    pub fn map_normalize(&self, word: SmolStr, min_votes: u8) -> SmolStr {
        if let Some((normalized_word, _)) = self.get_matching(word.as_str(), min_votes) {
            normalized_word.into()
        } else {
            word
        }
    }

    pub fn dump_values(&self) -> impl Iterator<Item = &(SmolStr, u8)> {
        self.mapping.values()
    }
}

// ===========================================================================
// Specifics DSL
// ===========================================================================

#[derive(Debug, Clone, Copy)]
pub enum SpecificsAction<'a> {
    Increase(SpecificsCond<'a>),
    Decrease(SpecificsCond<'a>),
    Fold(SpecificsCond<'a>),
}

impl<'a> SpecificsAction<'a> {
    #[must_use]
    pub fn cond(&self) -> &SpecificsCond<'a> {
        let (Self::Increase(condition) | Self::Decrease(condition) | Self::Fold(condition)) = self;
        condition
    }

    #[must_use]
    pub fn keyword(&self) -> &'a str {
        self.cond().with
    }
}

#[derive(Clone, Copy)]
pub struct SpecificsCond<'a> {
    pub with: &'a str,
    include_exclude: &'a str,
}

impl fmt::Debug for SpecificsCond<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.with)?;
        formatter.write_str(self.include_exclude)
    }
}

impl<'a> SpecificsCond<'a> {
    fn new_plain(keyword: &'a str) -> Self {
        Self {
            with: keyword,
            include_exclude: "",
        }
    }

    fn new(action: &'a str) -> Self {
        debug_assert!(!action.starts_with(['+', '-']));
        let (with, include_exclude) = action
            .split_once(['!', '?'])
            .and_then(|(keyword, _)| action.split_at_checked(keyword.len()))
            .unwrap_or((action, ""));

        Self {
            with,
            include_exclude,
        }
    }

    pub fn unless(&self) -> impl Iterator<Item = &str> {
        self.conditions()
            .filter_map(|(modifier, word)| (modifier == b'!').then_some(word))
    }

    pub fn only_if_all(&self) -> impl Iterator<Item = &str> {
        self.conditions()
            .filter_map(|(modifier, word)| (modifier == b'?').then_some(word))
    }

    pub fn matches(&self, callback: impl Fn(&str) -> bool + Copy) -> bool {
        self.only_if_all().all(callback) && !self.unless().any(callback)
    }

    fn conditions(&self) -> impl Iterator<Item = (u8, &str)> {
        let mut remaining = self.include_exclude;
        std::iter::from_fn(move || {
            if remaining.is_empty() {
                return None;
            }

            let modifier = remaining.as_bytes()[0];
            remaining = &remaining[1..];

            let end_index = remaining
                .bytes()
                .position(|byte| byte == b'?' || byte == b'!')
                .unwrap_or(remaining.len());

            let (word, rest) = remaining.split_at(end_index);
            remaining = rest;

            Some((modifier, word))
        })
    }
}

type SplitOffsets = Vec<u8>;

#[derive(Default)]
pub struct Specifics {
    combine: HashMap<SmolStr, String>,
    relate: HashMap<SmolStr, String>,
    split: HashMap<SmolStr, SplitOffsets>,
    bland: HashSet<SmolStr>,
}

impl Specifics {
    #[inline(never)]
    pub fn new(data_dir: &Path) -> io::Result<Self> {
        let mut parsed = Self::default();

        let specifics_source = Self::read_optional_file(data_dir.join("specific-keywords.txt"))?;
        let bland_source = Self::read_optional_file(data_dir.join("bland-keywords.txt"))?;

        let has_errors = parsed.parse_keywords(&specifics_source, &bland_source);
        if has_errors {
            tracing::error!("Found syntax errors in specific-keywords.txt");
        }

        Ok(parsed)
    }

    fn read_optional_file(path: std::path::PathBuf) -> io::Result<String> {
        if path.exists() {
            fs::read_to_string(&path)
        } else {
            tracing::warn!("{:?} not found", path);
            Ok(String::new())
        }
    }

    fn parse_keywords(&mut self, specifics_lines: &str, bland_lines: &str) -> bool {
        let mut has_errors = false;

        for line in specifics_lines.lines() {
            let line = line.trim_ascii();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if line.contains(" < ") {
                has_errors |= self.parse_relation(line, " < ", false, true);
            } else if line.contains(" & ") {
                has_errors |= self.parse_relation(line, " & ", true, true);
            } else if line.contains(" + ") {
                has_errors |= self.parse_relation(line, " + ", false, false);
            } else if line.contains('.') {
                has_errors |= self.parse_split(line);
            } else {
                tracing::error!("bad line: {}", line);
                has_errors = true;
            }
        }

        self.bland.extend(
            bland_lines
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(SmolStr::from),
        );

        has_errors
    }

    fn parse_relation(
        &mut self,
        line: &str,
        separator: &str,
        must_have_prefix: bool,
        is_relate: bool,
    ) -> bool {
        let Some((keyword_string, rest)) = line.split_once(separator) else {
            return true;
        };

        let keyword = SpecificsCond::new(keyword_string.trim_ascii_end());
        if keyword.with.is_empty() {
            tracing::error!("bad line (empty keyword): {}", line);
            return true;
        }

        let target_map = if is_relate {
            &mut self.relate
        } else {
            &mut self.combine
        };
        let list = target_map.entry(SmolStr::from(keyword.with)).or_default();
        list.reserve(rest.trim_ascii_start().len());

        let mut error_found = false;
        for word in rest.split([' ', ',']).filter(|segment| !segment.is_empty()) {
            let stem = word
                .trim_start_matches(['-', '+'])
                .split(['!', '?'])
                .next()
                .unwrap_or_default();

            if stem.is_empty()
                || stem == keyword.with
                || must_have_prefix != word.starts_with(['-', '+'])
            {
                tracing::error!("bad word '{}' in line: {}", word, line);
                error_found = true;
                continue;
            }

            if !list.is_empty() {
                list.push(',');
            }
            list.push_str(word);
            if !keyword.include_exclude.is_empty() {
                list.push_str(keyword.include_exclude);
            }
        }
        error_found
    }

    fn parse_split(&mut self, line: &str) -> bool {
        let mut previous_index = 0usize;
        let splits: Vec<u8> = line
            .bytes()
            .enumerate()
            .filter_map(|(index, byte)| (byte == b'.').then_some(index))
            .take(4)
            .map(|index| {
                let length = (index - previous_index) as u8;
                previous_index = index + 1;
                length
            })
            .collect();

        let keyword = line.replace('.', "-");
        if splits.is_empty() || keyword.is_empty() {
            tracing::error!("bad split line: {}", line);
            return true;
        }

        if self.split.insert(keyword.into(), splits).is_some() {
            tracing::error!("duplicate split line: {}", line);
            return true;
        }
        false
    }

    #[inline]
    #[must_use]
    pub fn get_combined<'s>(
        &'s self,
        word: &str,
    ) -> Option<(&'s str, impl Iterator<Item = SpecificsCond<'s>> + 's)> {
        let (key, value) = self.combine.get_key_value(word)?;
        Some((key.as_str(), value.split(',').map(SpecificsCond::new)))
    }

    #[inline]
    #[must_use]
    pub fn get_splits<'s>(
        &'s self,
        word: &str,
    ) -> Option<(&'s str, usize, impl Iterator<Item = &'s str> + 's)> {
        let (key, offsets_list) = self.split.get_key_value(word)?;
        let mut remaining = key.as_str();
        let mut offsets = offsets_list.clone().into_iter().map(usize::from);
        let part_count = 1 + offsets_list.len();

        Some((
            key.as_str(),
            part_count,
            std::iter::from_fn(move || {
                if remaining.is_empty() {
                    return None;
                }
                if let Some(offset) = offsets.next() {
                    let (part, rest) = remaining.split_at_checked(offset + 1)?;
                    remaining = rest;
                    let (part, _) = part.split_at_checked(offset)?;
                    Some(part)
                } else {
                    let part = remaining;
                    remaining = &remaining[..0];
                    Some(part)
                }
            }),
        ))
    }

    #[inline]
    #[must_use]
    pub fn get_relations<'s>(
        &'s self,
        word: &str,
    ) -> Option<(&'s str, impl Iterator<Item = SpecificsAction<'s>> + 's)> {
        let (key, value) = self.relate.get_key_value(word)?;
        Some((
            key.as_str(),
            value.split(',').filter_map(move |action| {
                let (prefix, suffix) = action.split_at_checked(1)?;
                Some(match prefix {
                    "+" => SpecificsAction::Increase(SpecificsCond::new(suffix)),
                    "-" => SpecificsAction::Decrease(SpecificsCond::new(suffix)),
                    _ => SpecificsAction::Fold(SpecificsCond::new(action)),
                })
            }),
        ))
    }

    pub fn extra_keywords(&self) -> impl Iterator<Item = SmolStr> + '_ {
        self.relate
            .keys()
            .cloned()
            .chain(self.combine.iter().flat_map(|(key1, rest)| {
                rest.split(',').map(move |key2| {
                    let key2 = key2
                        .trim_start_matches(['+', '-'])
                        .split(['!', '?'])
                        .next()
                        .unwrap();
                    SmolStr::from(format!("{key1}-{key2}"))
                })
            }))
    }

    pub fn is_bland<'s>(&'s self, keyword: &str) -> Option<SpecificsAction<'s>> {
        self.bland
            .get(keyword)
            .map(|key| SpecificsAction::Fold(SpecificsCond::new_plain(key.as_str())))
    }

    pub fn all_bland_keywords(&self) -> impl Iterator<Item = &str> + '_ {
        self.bland.iter().map(|string| string.as_str())
    }
}
