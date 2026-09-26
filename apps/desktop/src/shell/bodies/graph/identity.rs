//! Exact source identities joining the temporary graph fixture to indexed
//! declarations. Package aliases are admitted once in the background from
//! Cargo manifests; UI lookups use typed keys and exact relative paths.

use crate::model::pages::{DeclRef, PackageRef, SearchRow, SymbolRef};
use facet::graph::{NodeId, World};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub(super) struct ResolvedSymbol {
    pub symbol: SymbolRef,
    pub package: PackageRef,
    pub line: Option<u32>,
}

#[derive(Clone, Debug)]
struct PackageBinding {
    aliases: Vec<PackageRef>,
    fixture_root: PathBuf,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SourceKey {
    package: usize,
    file: PathBuf,
    name: String,
    line: u32,
}

pub(super) struct IdentityAdapter {
    aliases: BTreeMap<PackageRef, Vec<usize>>,
    sources: BTreeMap<SourceKey, Vec<NodeId>>,
    nodes: BTreeMap<NodeId, SourceKey>,
}

impl IdentityAdapter {
    /// Filesystem admission happens beside fixture parse/layout, off the UI.
    pub(super) fn load(world: &World, repo: &Path) -> Self {
        let bindings = world
            .packages
            .iter()
            .enumerate()
            .map(|(index, package)| {
                if package.external {
                    let release = format!("{}-{}", package.name, package.version);
                    let mut aliases = PackageRef::parse(&format!(
                        "pkg:cargo/{}@{}",
                        package.name, package.version
                    ))
                    .into_iter()
                    .collect::<Vec<_>>();
                    let cargo_home =
                        std::env::var_os("CARGO_HOME")
                            .map(PathBuf::from)
                            .or_else(|| {
                                std::env::var_os("HOME")
                                    .map(|home| PathBuf::from(home).join(".cargo"))
                            });
                    if let Some(home) = cargo_home
                        && let Ok(indexes) = std::fs::read_dir(home.join("registry/src"))
                    {
                        for directory in indexes.flatten() {
                            let root = directory.path().join(&release);
                            if manifest(&root).is_some_and(|(name, version)| {
                                name == package.name.as_ref()
                                    && version.as_deref() == Some(package.version.as_ref())
                            }) {
                                if let Ok(root) = root.canonicalize()
                                    && let Ok(alias) = PackageRef::parse(&root.to_string_lossy())
                                {
                                    aliases.push(alias);
                                }
                            }
                        }
                    }
                    Some(PackageBinding {
                        aliases,
                        fixture_root: PathBuf::from(release),
                    })
                } else {
                    let module = world
                        .modules
                        .iter()
                        .find(|module| module.pkg as usize == index)?;
                    let source = repo.join(module.file.as_ref());
                    let root = source.parent()?.ancestors().find(|candidate| {
                        manifest(candidate).is_some_and(|(name, _)| name == package.name.as_ref())
                    })?;
                    let root = root.canonicalize().ok()?;
                    let alias = PackageRef::parse(&root.to_string_lossy()).ok()?;
                    let repo = repo.canonicalize().ok()?;
                    let fixture_root = root.strip_prefix(repo).ok()?.to_path_buf();
                    Some(PackageBinding {
                        aliases: vec![alias],
                        fixture_root,
                    })
                }
            })
            .collect::<Vec<_>>();
        Self::admit(world, &bindings)
    }

    fn admit(world: &World, bindings: &[Option<PackageBinding>]) -> Self {
        let mut aliases: BTreeMap<PackageRef, Vec<usize>> = BTreeMap::new();
        for (package, binding) in bindings.iter().enumerate() {
            if let Some(binding) = binding {
                for alias in &binding.aliases {
                    aliases.entry(alias.clone()).or_default().push(package);
                }
            }
        }
        let mut sources: BTreeMap<SourceKey, Vec<NodeId>> = BTreeMap::new();
        let mut nodes = BTreeMap::new();
        for (id, node) in world.nodes.iter().enumerate() {
            let Some(binding) = bindings.get(node.pkg as usize).and_then(Option::as_ref) else {
                continue;
            };
            let file = node
                .file
                .as_ref()
                .unwrap_or(&world.modules[node.module as usize].file);
            let Some(file) = normalized(Path::new(file.as_ref())) else {
                continue;
            };
            let Ok(file) = file.strip_prefix(&binding.fixture_root) else {
                continue;
            };
            let Ok(id) = u32::try_from(id) else { continue };
            let key = SourceKey {
                package: node.pkg as usize,
                file: file.to_path_buf(),
                name: node.name.to_string(),
                line: node.line,
            };
            sources.entry(key.clone()).or_default().push(id);
            nodes.insert(id, key);
        }
        Self {
            aliases,
            sources,
            nodes,
        }
    }

    #[cfg(test)]
    pub(super) fn synthetic(world: &World, package: PackageRef) -> Self {
        Self::admit(
            world,
            &[Some(PackageBinding {
                aliases: vec![package],
                fixture_root: PathBuf::new(),
            })],
        )
    }

    pub(super) fn candidates(&self, decl: &DeclRef, package: &PackageRef) -> Vec<NodeId> {
        let (Some(line), Some(path), Some(packages)) =
            (decl.line, decl.path.as_deref(), self.aliases.get(package))
        else {
            return Vec::new();
        };
        let Some(file) = normalized(Path::new(path)) else {
            return Vec::new();
        };
        let file = if file.is_absolute() && package.is_local() {
            let Some(root) = normalized(Path::new(package.as_str())) else {
                return Vec::new();
            };
            let Ok(file) = file.strip_prefix(root) else {
                return Vec::new();
            };
            file.to_path_buf()
        } else {
            file
        };
        packages
            .iter()
            .flat_map(|&package| {
                self.sources
                    .get(&SourceKey {
                        package,
                        file: file.clone(),
                        name: decl.name.to_string(),
                        line,
                    })
                    .into_iter()
                    .flatten()
                    .copied()
            })
            .collect()
    }

    pub(super) fn resolve(
        &self,
        node: NodeId,
        rows: &[SearchRow],
    ) -> Result<ResolvedSymbol, MatchFailure> {
        let source = self.nodes.get(&node).ok_or(MatchFailure::MissingFixture)?;
        if self
            .sources
            .get(source)
            .is_none_or(|nodes| nodes.as_slice() != [node])
        {
            return Err(MatchFailure::AmbiguousFixture);
        }
        let matches = rows
            .iter()
            .filter_map(|row| {
                let package = PackageRef::parse(row.package.as_deref()?).ok()?;
                (self.candidates(&row.decl, &package).as_slice() == [node])
                    .then_some((row, package))
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [(row, package)] => Ok(ResolvedSymbol {
                symbol: row.decl.coordinate.clone(),
                package: package.clone(),
                line: row.decl.line,
            }),
            [] => Err(MatchFailure::MissingIndex),
            _ => Err(MatchFailure::AmbiguousIndex),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MatchFailure {
    MissingFixture,
    AmbiguousFixture,
    MissingIndex,
    AmbiguousIndex,
}

impl std::fmt::Display for MatchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::MissingFixture => "This fixture symbol has no admitted package/source identity",
            Self::AmbiguousFixture => "Several fixture symbols share this exact source identity",
            Self::MissingIndex => "The local index has no exact match for this fixture symbol",
            Self::AmbiguousIndex => {
                "The local index has several exact matches for this fixture symbol"
            }
        })
    }
}

fn manifest(root: &Path) -> Option<(String, Option<String>)> {
    let source = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let manifest = toml::from_str::<toml::Value>(&source).ok()?;
    let package = manifest.get("package")?;
    Some((
        package.get("name")?.as_str()?.to_owned(),
        package
            .get("version")
            .and_then(toml::Value::as_str)
            .map(str::to_owned),
    ))
}

fn normalized(path: &Path) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !result.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            component => result.push(component.as_os_str()),
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use facet::graph::{Kind, Module, Node, Package};
    use std::sync::Arc;

    fn small(duplicate: bool) -> (World, IdentityAdapter) {
        let packages = vec![
            Package {
                name: "one".into(),
                version: "1.0.0".into(),
                yours: true,
                external: false,
                deps: vec![],
            },
            Package {
                name: "two".into(),
                version: "2.0.0".into(),
                yours: false,
                external: true,
                deps: vec![],
            },
        ];
        let modules = vec![
            Module {
                pkg: 0,
                path: "".into(),
                file: "one/src/lib.rs".into(),
            },
            Module {
                pkg: 1,
                path: "".into(),
                file: "two-2.0.0/src/lib.rs".into(),
            },
        ];
        let mut nodes = vec![
            Node::new(Kind::Struct, "Value", 0, 0),
            Node::new(Kind::Struct, "Value", 1, 1),
        ];
        for node in &mut nodes {
            node.line = 7;
        }
        if duplicate {
            nodes.push(nodes[0].clone());
        }
        let world = World::new(packages, modules, nodes, vec![]).expect("world");
        let adapter = IdentityAdapter::admit(
            &world,
            &[
                Some(PackageBinding {
                    aliases: vec![PackageRef::parse("/index/one").expect("package")],
                    fixture_root: "one".into(),
                }),
                Some(PackageBinding {
                    aliases: vec![PackageRef::parse("pkg:cargo/two@2.0.0").expect("package")],
                    fixture_root: "two-2.0.0".into(),
                }),
            ],
        );
        (world, adapter)
    }

    fn row(package: &str, path: &str, line: u32) -> SearchRow {
        let coordinate = format!("{package}::{path}:{line}::Value");
        SearchRow {
            rank: 0,
            decl: DeclRef::from_label(&coordinate, None, None, Some((path, line))).expect("decl"),
            package: Some(Arc::from(package)),
            score: crate::model::pages::Known::Known(0),
            signature: crate::model::pages::Known::unknown(
                crate::model::pages::GapReason::NotCaptured,
                "",
            ),
            snippet: None,
            reason: crate::model::pages::MatchReason::ExactName,
        }
    }

    #[test]
    fn exact_typed_source_keys_distinguish_package_version_path_and_line() {
        let (_, adapter) = small(false);
        let right = row("/index/one", "src/lib.rs", 7);
        assert_eq!(
            adapter.resolve(0, &[right.clone()]).expect("match").symbol,
            right.decl.coordinate
        );
        for wrong in [
            row("pkg:cargo/two@2.0.0", "src/lib.rs", 7),
            row("pkg:cargo/two@2.0.1", "src/lib.rs", 7),
            row("/index/one", "other/src/lib.rs", 7),
            row("/index/one", "src/lib.rs", 8),
        ] {
            assert!(matches!(
                adapter.resolve(0, &[wrong]),
                Err(MatchFailure::MissingIndex)
            ));
        }
        let absolute = row("/index/one", "/index/one/src/lib.rs", 7);
        assert!(adapter.resolve(0, &[absolute]).is_ok());
    }

    #[test]
    fn duplicate_fixture_or_index_sources_are_never_arbitrarily_chosen() {
        let (_, adapter) = small(false);
        let right = row("/index/one", "src/lib.rs", 7);
        assert!(matches!(
            adapter.resolve(0, &[right.clone(), right.clone()]),
            Err(MatchFailure::AmbiguousIndex)
        ));
        let (_, duplicate) = small(true);
        assert!(matches!(
            duplicate.resolve(0, &[right]),
            Err(MatchFailure::AmbiguousFixture)
        ));
        assert!(matches!(
            adapter.resolve(99, &[]),
            Err(MatchFailure::MissingFixture)
        ));
    }
}
