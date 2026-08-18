//! Best-effort SPDX identifier sniffing for standalone `LICENSE`/`LICENCE`
//! files — shared by ecosystems whose native manifest format carries no
//! license field of its own at all (Go's `go.mod`), or whose build-descriptor
//! parsers only sometimes carry one (C/C++'s `vcpkg.json`/`conanfile.*`/
//! `meson.build` do; `CMakeLists.txt` and a plain `.pc`/`.pc.in` file do not —
//! see `cpp::manifest::cmake`/`pkgconfig`).
//!
//! Conservative by design: a license file is *evidence a license exists*
//! even when its exact identifier can't be pinned down, so a caller that
//! finds one of [`LICENSE_FILENAMES`] should always record
//! `has_license_file: true`, and only set `license` when [`detect_spdx`]
//! returns `Some`. [`detect_spdx`] never fabricates an identifier from the
//! filename alone — it either recognizes an unambiguous, well-known
//! signature in the *content*, or returns `None`. A wrong SPDX string is
//! worse than none (REGISTRYLESS facets-merge task): `license: None` with
//! `has_license_file: true` is always the safe fallback.

/// Root-level standalone license filenames recognized as manifest
/// candidates, matched exactly (case-insensitively) by
/// `server::coordination::indexing::facets::matches_candidate` — there is no
/// wildcard/prefix matching, so uncommon spellings (`LICENSE-MIT`,
/// `LICENSE.rst`, `UNLICENSE`, …) are not covered. This is a deliberately
/// small, curated list of the spellings that actually dominate real-world
/// repositories.
pub const LICENSE_FILENAMES: &[&str] = &[
    "LICENSE",
    "LICENSE.txt",
    "LICENSE.md",
    "LICENCE",
    "LICENCE.txt",
    "LICENCE.md",
    "COPYING",
    "COPYING.txt",
];

/// Detect an SPDX license expression from standalone license-file text.
///
/// Returns `None` when the content doesn't match one of the signatures
/// below — the file is still real evidence of a license (the caller should
/// set `has_license_file: true` regardless of this function's result), just
/// not one this conservative sniffer can name with confidence.
pub fn detect_spdx(text: &str) -> Option<String> {
    // Highest confidence: an explicit machine-readable header, verbatim.
    // Checked only in the first few lines — a `SPDX-License-Identifier:`
    // line buried deep in prose (e.g. quoted in a CHANGELOG) is not this
    // file's own declaration.
    for line in text.lines().take(20) {
        if let Some(rest) = line.trim_start().strip_prefix("SPDX-License-Identifier:") {
            let expr = rest.trim();
            if !expr.is_empty() {
                return Some(expr.to_owned());
            }
        }
    }

    // Whitespace-normalized (single spaces, lowercase) so wrapped lines and
    // inconsistent indentation don't defeat a substring match.
    let normalized: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    // Each entry: ALL marker substrings must be present for the SPDX id to
    // be reported. Ordered most-specific-first (checked as a `find`, so the
    // first whole-signature match wins) so e.g. LGPL is tried before GPL,
    // and BSD-3-Clause (which is a superset of BSD-2-Clause's clause) before
    // BSD-2-Clause.
    const SIGNATURES: &[(&[&str], &str)] = &[
        (
            &["gnu lesser general public license", "version 3"],
            "LGPL-3.0-only",
        ),
        (
            &["gnu lesser general public license", "version 2.1"],
            "LGPL-2.1-only",
        ),
        (
            &["gnu affero general public license", "version 3"],
            "AGPL-3.0-only",
        ),
        (&["gnu general public license", "version 3"], "GPL-3.0-only"),
        (&["gnu general public license", "version 2"], "GPL-2.0-only"),
        (&["mozilla public license", "2.0"], "MPL-2.0"),
        (&["apache license", "version 2.0"], "Apache-2.0"),
        (
            &["redistributions in binary form", "neither the name of"],
            "BSD-3-Clause",
        ),
        (&["redistributions in binary form"], "BSD-2-Clause"),
        (
            &[
                "permission to use, copy, modify, and/or distribute this software",
                "the software is provided \"as is\"",
            ],
            "ISC",
        ),
        (
            &[
                "permission is hereby granted, free of charge, to any person obtaining a copy of this software",
            ],
            "MIT",
        ),
        (
            &["this is free and unencumbered software released into the public domain"],
            "Unlicense",
        ),
    ];

    SIGNATURES
        .iter()
        .find(|(markers, _)| markers.iter().all(|m| normalized.contains(m)))
        .map(|(_, spdx)| (*spdx).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spdx_header_wins_verbatim() {
        let text = "SPDX-License-Identifier: MIT OR Apache-2.0\n\nSome other text.";
        assert_eq!(detect_spdx(text).as_deref(), Some("MIT OR Apache-2.0"));
    }

    #[test]
    fn spdx_header_deep_in_file_is_not_matched() {
        // Only the first 20 lines are treated as the file's own header.
        let mut text = String::new();
        for _ in 0..30 {
            text.push_str("filler line\n");
        }
        text.push_str("SPDX-License-Identifier: MIT\n");
        assert_eq!(detect_spdx(&text), None);
    }

    #[test]
    fn mit_boilerplate_detected() {
        let text = "MIT License\n\nCopyright (c) 2024 Example\n\n\
Permission is hereby granted, free of charge, to any person obtaining a copy of this software \
and associated documentation files (the \"Software\"), to deal in the Software without restriction...";
        assert_eq!(detect_spdx(text).as_deref(), Some("MIT"));
    }

    #[test]
    fn apache2_boilerplate_detected() {
        let text = "\n                                 Apache License\n                           Version 2.0, January 2004\n";
        assert_eq!(detect_spdx(text).as_deref(), Some("Apache-2.0"));
    }

    #[test]
    fn bsd3_vs_bsd2_discriminated() {
        let bsd3 = "Redistributions in binary form must reproduce the above copyright notice. \
Neither the name of the copyright holder nor the names of its contributors may be used.";
        assert_eq!(detect_spdx(bsd3).as_deref(), Some("BSD-3-Clause"));

        let bsd2 = "Redistributions in binary form must reproduce the above copyright notice, \
this list of conditions and the following disclaimer.";
        assert_eq!(detect_spdx(bsd2).as_deref(), Some("BSD-2-Clause"));
    }

    #[test]
    fn isc_boilerplate_detected() {
        let text = "Permission to use, copy, modify, and/or distribute this software for any \
purpose with or without fee is hereby granted. THE SOFTWARE IS PROVIDED \"AS IS\" AND THE \
AUTHOR DISCLAIMS ALL WARRANTIES.";
        assert_eq!(detect_spdx(text).as_deref(), Some("ISC"));
    }

    #[test]
    fn gpl_version_discriminated() {
        let gpl3 = "GNU GENERAL PUBLIC LICENSE\nVersion 3, 29 June 2007";
        assert_eq!(detect_spdx(gpl3).as_deref(), Some("GPL-3.0-only"));

        let gpl2 = "GNU GENERAL PUBLIC LICENSE\nVersion 2, June 1991";
        assert_eq!(detect_spdx(gpl2).as_deref(), Some("GPL-2.0-only"));
    }

    #[test]
    fn lgpl_version_discriminated() {
        let lgpl3 = "GNU LESSER GENERAL PUBLIC LICENSE\nVersion 3, 29 June 2007";
        assert_eq!(detect_spdx(lgpl3).as_deref(), Some("LGPL-3.0-only"));

        let lgpl21 = "GNU LESSER GENERAL PUBLIC LICENSE\nVersion 2.1, February 1999";
        assert_eq!(detect_spdx(lgpl21).as_deref(), Some("LGPL-2.1-only"));
    }

    #[test]
    fn mpl2_boilerplate_detected() {
        let text = "Mozilla Public License, v. 2.0. If a copy of the MPL was not distributed...";
        assert_eq!(detect_spdx(text).as_deref(), Some("MPL-2.0"));
    }

    #[test]
    fn unlicense_boilerplate_detected() {
        let text = "This is free and unencumbered software released into the public domain.";
        assert_eq!(detect_spdx(text).as_deref(), Some("Unlicense"));
    }

    #[test]
    fn unrecognized_content_returns_none() {
        assert_eq!(
            detect_spdx("All rights reserved. Contact us for terms."),
            None
        );
        assert_eq!(detect_spdx(""), None);
    }

    #[test]
    fn license_filenames_list_is_the_common_curated_set() {
        assert!(LICENSE_FILENAMES.contains(&"LICENSE"));
        assert!(LICENSE_FILENAMES.contains(&"LICENCE"));
        assert!(LICENSE_FILENAMES.contains(&"COPYING"));
        assert_eq!(LICENSE_FILENAMES.len(), 8);
    }
}
