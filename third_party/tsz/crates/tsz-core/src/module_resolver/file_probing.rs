//! File probing and extension candidate logic for module resolution.
//!
//! This module contains the filesystem probing methods that try various
//! extension substitutions, suffix combinations, and directory index
//! fallbacks to find the actual file backing a module specifier.

use super::ModuleResolver;
use super::request_types::{ModuleExtension, PackageType};
use crate::config::ModuleResolutionKind;
use crate::module_resolver_helpers::*;
use std::path::{Path, PathBuf};

impl<'fs> ModuleResolver<'fs> {
    // =========================================================================
    // File probing methods
    // =========================================================================

    /// Try to resolve a file with various extensions.
    ///
    /// `package_type` is the type of the package CONTAINING `path` (the
    /// target's `package.json#type` for external lookups, the importer's
    /// own package type for in-tree lookups). For `Node16`/`NodeNext` this
    /// drives whether `.mts`/`.d.mts` or `.cts`/`.d.cts` are tried first; in
    /// other resolution modes it is ignored. Use `None` when no package
    /// context applies (path mapping, baseUrl, classic, bundler bare).
    pub(super) fn try_file(
        &self,
        path: &Path,
        package_type: Option<PackageType>,
    ) -> Option<PathBuf> {
        self.try_file_inner(
            path,
            package_type,
            true,
            !matches!(
                self.resolution_kind,
                ModuleResolutionKind::Node16 | ModuleResolutionKind::NodeNext,
            ),
        )
    }

    /// Like [`Self::try_file`], but does NOT try directory index resolution
    /// (`path/index.{ext}`). Used for ESM packages in Node16/NodeNext where
    /// directory index resolution is not allowed by Node.js.
    pub(super) fn try_file_no_index(
        &self,
        path: &Path,
        package_type: Option<PackageType>,
    ) -> Option<PathBuf> {
        self.try_file_inner(path, package_type, false, false)
    }

    /// Shared file probing with separate directory-index and implicit-extension
    /// policies. Node16/NodeNext ESM contexts prohibit both; a relative request
    /// supplies its actual import/require mode independently of package type.
    /// The explicit-extension stages — normalization,
    /// arbitrary-extension declaration probing, `node16_extension_substitution`,
    /// the `rewriteRelativeImportExtensions` declaration remap, and the
    /// original-extension fallback — remain shared by every caller.
    fn try_file_inner(
        &self,
        path: &Path,
        package_type: Option<PackageType>,
        try_index: bool,
        implicit_extensions: bool,
    ) -> Option<PathBuf> {
        // Canonicalize `.`/`..` before probing so the returned `PathBuf` is
        // the same across alias branches that point at the same file.
        let path = &normalize_path_segments(path);
        let suffixes = &self.module_suffixes;
        if let Some(extension) = path.extension().and_then(|ext| ext.to_str())
            && split_path_extension(path).is_none()
        {
            // Always probe for .d.*.ts declaration files regardless of allowArbitraryExtensions.
            // When the flag is off, the caller (lookup) emits TS6263 for the resolved file.
            if let Some(resolved) = try_arbitrary_extension_declaration_with(path, extension, |p| {
                self.is_file(p)
            }) {
                return Some(resolved);
            }
            // A dotted basename such as `app.component` can still name
            // `app.component.ts`. TypeScript appends implementation suffixes
            // after the declaration-companion probe only outside ESM mode.
            if !implicit_extensions {
                return None;
            }
        }
        if let Some((base, extension)) = split_path_extension(path) {
            // A `.json` specifier's sibling `.d.json.ts` declaration file
            // takes priority over the JSON file's own literal shape,
            // unconditionally — mirrors tsc's `tryAddingExtensions`
            // `Extension.Json` case (Declaration tried before Json) and is
            // independent of both `resolveJsonModule` and
            // `allowArbitraryExtensions`. Probed regardless of the flag, like
            // the unknown-extension declaration probe above: when the flag
            // is off, `is_arbitrary_extension_declaration` in `mod.rs` still
            // emits TS6263 for the resolved declaration (verified against
            // pinned `typescript@7.0.2`: both flag states resolve to the
            // declaration, and only the diagnostic differs).
            if extension == "json"
                && let Some(resolved) = try_arbitrary_extension_declaration_with(path, extension, |p| {
                    self.is_file(p)
                })
            {
                return Some(resolved);
            }

            // Try extension substitution (.js -> .ts/.tsx/.d.ts) for all resolution modes.
            // TypeScript resolves `.js` imports to `.ts` sources in all modes.
            if let Some(rewritten) = node16_extension_substitution(path, extension) {
                for candidate in &rewritten {
                    if let Some(resolved) = try_file_with_suffixes_with(candidate, suffixes, |p| {
                        self.is_file(p)
                    }) {
                        return Some(resolved);
                    }
                }
            }

            // When rewriteRelativeImportExtensions is true, .ts/.tsx/.mts/.cts imports
            // should resolve to their declaration file equivalents (.d.ts/.d.mts/.d.cts).
            if self.rewrite_relative_import_extensions
                && let Some(decl_ext) = ts_extension_to_declaration(extension)
            {
                let candidate = base.with_extension(decl_ext);
                if let Some(resolved) = try_file_with_suffixes_with(&candidate, suffixes, |p| {
                    self.is_file(p)
                }) {
                    return Some(resolved);
                }
            }

            // Fall back to the original extension (e.g., literal .js file)
            if let Some(resolved) = try_file_with_suffixes_and_extension_with(
                &base,
                extension,
                suffixes,
                |p| self.is_file(p),
            ) {
                return Some(resolved);
            }

            // A relative specifier naming a TS implementation extension
            // (`./tsx.ts`, `./plain.tsx`, `./mts1.mts`) resolves like tsc's
            // allowImportingTsExtensions probing: when the exact file is
            // missing, strip the extension and try the mode candidate list
            // ('./tsx.ts' -> tsx.tsx, './dts.ts' -> dts.d.ts, './js1.ts' ->
            // js1.js under allowJs). `.mts`/`.cts` specifiers stay within
            // their module-format pair.
            let ts_impl_fallback: Option<&[&str]> = match extension {
                "ts" | "tsx" => Some(self.extension_candidates_for_package_type(package_type)),
                "mts" => Some(if self.allow_js {
                    &["d.mts", "mjs"]
                } else {
                    &["d.mts"]
                }),
                "cts" => Some(if self.allow_js {
                    &["d.cts", "cjs"]
                } else {
                    &["d.cts"]
                }),
                _ => None,
            };
            if let Some(fallback_extensions) = ts_impl_fallback {
                for ext in fallback_extensions {
                    if let Some(resolved) =
                        try_file_with_suffixes_and_extension_with(&base, ext, suffixes, |p| {
                            self.is_file(p)
                        })
                    {
                        return Some(resolved);
                    }
                }
            }

            return None;
        }

        let extensions = self.extension_candidates_for_package_type(package_type);
        for ext in extensions {
            if let Some(resolved) =
                try_file_with_suffixes_and_extension_with(path, ext, suffixes, |p| self.is_file(p))
            {
                return Some(resolved);
            }
        }

        if try_index {
            let index = path.join("index");
            for ext in extensions {
                if let Some(resolved) = try_file_with_suffixes_and_extension_with(
                    &index,
                    ext,
                    suffixes,
                    |p| self.is_file(p),
                ) {
                    return Some(resolved);
                }
            }
        }

        None
    }

    pub(super) const fn extension_candidates_for_package_type(
        &self,
        package_type: Option<PackageType>,
    ) -> &'static [&'static str] {
        match self.resolution_kind {
            ModuleResolutionKind::Node16 | ModuleResolutionKind::NodeNext => match package_type {
                Some(PackageType::Module) => {
                    if self.allow_js {
                        &NODE16_MODULE_ALLOWJS_EXTENSION_CANDIDATES
                    } else {
                        &NODE16_MODULE_EXTENSION_CANDIDATES
                    }
                }
                Some(PackageType::CommonJs) => {
                    if self.allow_js {
                        &NODE16_COMMONJS_ALLOWJS_EXTENSION_CANDIDATES
                    } else {
                        &NODE16_COMMONJS_EXTENSION_CANDIDATES
                    }
                }
                None => {
                    if self.allow_js {
                        TS_JS_EXTENSION_CANDIDATES
                    } else {
                        TS_EXTENSION_CANDIDATES
                    }
                }
            },
            ModuleResolutionKind::Classic => {
                if self.allow_js {
                    TS_JS_EXTENSION_CANDIDATES
                } else {
                    CLASSIC_EXTENSION_CANDIDATES
                }
            }
            _ => {
                if self.allow_js {
                    TS_JS_EXTENSION_CANDIDATES
                } else {
                    TS_EXTENSION_CANDIDATES
                }
            }
        }
    }

    pub(super) fn try_directory(
        &self,
        path: &Path,
        package_type: Option<PackageType>,
    ) -> Option<PathBuf> {
        // Normalize before `is_dir()` so the existence probe is robust to
        // non-existent intermediates the host OS cannot walk, and so the
        // returned `PathBuf` is canonical (e.g. typesVersions `../` joins
        // produce `ts3.1/../ts3.1/..` otherwise).
        let path = &normalize_path_segments(path);
        if !self.is_dir(path) {
            return None;
        }

        let package_json_path = path.join("package.json");
        if self.is_file(&package_json_path)
            && let Ok(pj) = self.read_package_json(&package_json_path)
        {
            // The directory's own `package.json#type` determines the
            // extension-candidate priority for `main` / `types` resolution
            // inside this directory; the caller-supplied `package_type`
            // only acts as an inherited fallback when this nested
            // package.json has no `"type"` field.
            let dir_package_type = self.target_package_type_from_json(&pj, package_type);
            let types = pj
                .types
                .clone()
                .or_else(|| pj.typings.clone())
                .filter(|types| !types.trim().is_empty());

            // Apply typesVersions before the bare types/typings path. tsc's
            // loadNodeModuleFromDirectoryWorker consults typesVersions when
            // resolving a directory via package.json, matching the types field
            // value (defaulting to "index") as the subpath. Without this, a
            // relative import like `../` into a package with typesVersions
            // bypasses the redirect, producing a divergent resolution from tsc.
            if let Some(types_versions) = &pj.types_versions {
                let subpath = types.as_deref().unwrap_or("index");
                if let Some(resolved) =
                    self.resolve_types_versions(path, subpath, types_versions, dir_package_type)
                {
                    return Some(resolved);
                }
            }

            if let Some(types) = types {
                let types_path = path.join(&types);
                if let Some(resolved) = self.try_types_entry(&types_path, dir_package_type) {
                    return Some(resolved);
                }
            }
            if let Some(main) = &pj.main {
                let main_path = path.join(main);
                if let Some(resolved) = self.try_file(&main_path, dir_package_type) {
                    return Some(resolved);
                }
            }
            let index = path.join("index");
            return self.try_file(&index, dir_package_type);
        }

        let index = path.join("index");
        self.try_file(&index, package_type)
    }

    /// Try to resolve a path as a file or directory
    pub(super) fn try_file_or_directory(
        &self,
        path: &Path,
        package_type: Option<PackageType>,
    ) -> Option<PathBuf> {
        // Try as file first
        if let Some(resolved) = self.try_file(path, package_type) {
            return Some(resolved);
        }

        self.try_directory(path, package_type)
    }

    pub(super) fn try_file_or_directory_with_mode(
        &self,
        path: &Path,
        package_type: Option<PackageType>,
        implicit_extensions: bool,
    ) -> Option<PathBuf> {
        self.try_file_inner(path, package_type, true, implicit_extensions)
            .or_else(|| self.try_directory(path, package_type))
    }

    /// Resolve an exports target without Node16 extension substitution.
    ///
    /// Explicit extensions must exist exactly; extensionless targets follow normal lookup.
    pub(super) fn try_export_target(
        &self,
        path: &Path,
        package_type: Option<PackageType>,
    ) -> Option<PathBuf> {
        let path = &normalize_path_segments(path);
        if let Some(extension) = path.extension().and_then(|ext| ext.to_str()) {
            if split_path_extension(path).is_some() {
                // For JS export targets, try declaration substitution first.
                // tsc always prefers .d.ts/.ts/.tsx over .js when resolving
                // conditional export targets (output-to-source remapping).
                if let Some(rewritten) = node16_extension_substitution(path, extension) {
                    for candidate in &rewritten {
                        if let Some(resolved) =
                            try_file_with_suffixes_with(candidate, &self.module_suffixes, |p| {
                                self.is_file(p)
                            })
                        {
                            return Some(resolved);
                        }
                    }
                }
                // Fall back to the original file if no declaration substitute exists
                if self.is_file(path) {
                    return Some(path.to_path_buf());
                }
                return None;
            }
            // Always probe for .d.*.ts declaration files regardless of allowArbitraryExtensions.
            // When the flag is off, the caller (lookup) emits TS6263 for the resolved file.
            if let Some(resolved) = try_arbitrary_extension_declaration_with(path, extension, |p| {
                self.is_file(p)
            }) {
                return Some(resolved);
            }
            return None;
        }

        // In the `exports`/`imports`-aware modes (Node16, NodeNext, Bundler),
        // extensionless runtime targets (e.g. from `"./": "./"` directory
        // exports or a wildcard `"#core/*": "./src/core/*"` whose specifier
        // omits the extension) must NOT gain an extension or a directory
        // `index` lookup. Node.js resolves `exports`/`imports` targets via
        // PACKAGE_TARGET_RESOLVE, which returns the target verbatim — no
        // extension substitution, no directory index. tsc mirrors this for
        // these three modes: it only remaps a target carrying an explicit
        // `.js`/`.mjs`/`.cjs` extension to its `.ts` sibling (handled above),
        // and otherwise refuses to invent an extension for an extensionless
        // target. Without this guard `import "pkg/other"` would silently
        // resolve to `pkg/other.ts`/`pkg/other/index.ts`, contradicting both
        // the runtime and tsc.
        //
        // The legacy `Node` (node10) and `Classic` modes predate the spec'd
        // target algorithm and DO apply classic file/directory probing to an
        // extensionless `exports` target (verified against tsc with
        // `resolvePackageJsonExports`), so they fall through to the normal
        // file/directory lookup below.
        let skip_extension_probing = matches!(
            self.resolution_kind,
            ModuleResolutionKind::Node16
                | ModuleResolutionKind::NodeNext
                | ModuleResolutionKind::Bundler
        );
        if skip_extension_probing {
            return None;
        }
        if let Some(resolved) = self.try_file(path, package_type) {
            return Some(resolved);
        }
        if self.is_dir(path) {
            let index = path.join("index");
            return self.try_file(&index, package_type);
        }
        None
    }

    pub(super) fn try_types_entry(
        &self,
        path: &Path,
        package_type: Option<PackageType>,
    ) -> Option<PathBuf> {
        let path = &normalize_path_segments(path);
        if let Some(resolved) = resolve_explicit_unknown_extension_with(path, |p| self.is_file(p)) {
            return Some(resolved);
        }

        if let Some((base, extension)) = split_path_extension(path) {
            if let Some(rewritten) = node16_extension_substitution(path, extension) {
                for candidate in &rewritten {
                    if let Some(resolved) = try_file_with_suffixes_with(
                        candidate,
                        &self.module_suffixes,
                        |p| self.is_file(p),
                    ) {
                        return Some(resolved);
                    }
                }
            }

            let explicit_extension = ModuleExtension::from_path(path);
            if matches!(
                explicit_extension,
                ModuleExtension::Ts
                    | ModuleExtension::Tsx
                    | ModuleExtension::Dts
                    | ModuleExtension::DmTs
                    | ModuleExtension::DCts
                    | ModuleExtension::Mts
                    | ModuleExtension::Cts
            ) {
                return try_file_with_suffixes_and_extension_with(
                    &base,
                    extension,
                    &self.module_suffixes,
                    |p| self.is_file(p),
                );
            }

            return None;
        }

        self.try_file_or_directory(path, package_type)
    }
}
