//! Token-aware production Rust source gates.

mod allows;
mod constructs;
mod kernels;
mod ownership;
mod token;

use crate::{SourceFile, Violation};

use allows::broad_allows;
use constructs::forbidden_constructs;
use kernels::duplicate_path_copy_kernels;
pub use ownership::validate_architecture_guards;
use token::{
    RustToken, attribute_is_test_only, bracket_attribute, has_extension, in_ranges, is_test_path,
    matching_delimiters, test_only_ranges, tokenize_rust,
};

/// Scans production Rust snapshots for forbidden panic-style constructs,
/// unjustifiably broad lint allowances, and duplicate path-copy node kernels.
#[must_use]
pub fn validate_rust_sources(files: &[SourceFile]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for file in files {
        if is_test_path(&file.path) || !has_extension(&file.path, "rs") {
            continue;
        }
        let tokens = tokenize_rust(&file.contents);
        let test_ranges = test_only_ranges(&tokens);
        violations.extend(forbidden_constructs(file, &tokens, &test_ranges));
        violations.extend(broad_allows(file, &tokens, &test_ranges));
    }
    violations.extend(duplicate_path_copy_kernels(files));
    violations
}
