//! Duplicate path-copy kernel checks.

use super::{
    SourceFile, has_extension, in_ranges, is_test_path, matching_delimiters, test_only_ranges,
    tokenize_rust,
};
use crate::Violation;

pub(super) fn duplicate_path_copy_kernels(files: &[SourceFile]) -> Vec<Violation> {
    let mut candidates = Vec::new();
    for file in files {
        if is_test_path(&file.path)
            || !has_extension(&file.path, "rs")
            || is_shared_kernel_path(&file.path)
        {
            continue;
        }
        let tokens = tokenize_rust(&file.contents);
        let pairs = matching_delimiters(&tokens);
        let test_ranges = test_only_ranges(&tokens);
        for (index, token) in tokens.iter().enumerate() {
            if token.text != "struct" || in_ranges(token.start, &test_ranges) {
                continue;
            }
            let Some(name) = tokens.get(index + 1).map(|token| token.text.clone()) else {
                continue;
            };
            let Some(open) = tokens[index + 2..]
                .iter()
                .position(|token| token.text == "{")
                .map(|offset| index + 2 + offset)
            else {
                continue;
            };
            let Some(close) = pairs.get(&open).copied() else {
                continue;
            };
            let body = &tokens[open + 1..close];
            let recursive = body.windows(4).any(|window| {
                window[0].text == "Arc"
                    && window[1].text == "<"
                    && (window[2].text == "Self" || window[2].text == name)
                    && window[3].text == ">"
            });
            let tree_shape = ["left", "right", "children", "parent"]
                .iter()
                .filter(|field| body.iter().any(|token| token.text == **field))
                .count()
                >= 2;
            if recursive && tree_shape {
                candidates.push((file.path.clone(), name, token.line));
            }
        }
    }

    candidates
        .into_iter()
        .map(|(path, name, line)| Violation::DuplicatePathCopyKernel { path, name, line })
        .collect()
}

fn is_shared_kernel_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.contains("/crates/version/src/")
        || normalized.starts_with("crates/version/src/")
        || normalized == "crates/version/src"
}
