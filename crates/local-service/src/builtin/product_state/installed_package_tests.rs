#[cfg(unix)]
#[test]
fn installed_npm_and_virtualenv_packages_answer_public_package_routes() {
    let root = fixture("installed-package-routes");
    let npm = root.join("frontend/node_modules/vue");
    let python = root.join("backend/.venv/lib/python3.14/site-packages");
    fs::create_dir_all(&npm).unwrap();
    fs::create_dir_all(python.join("fastapi-0.162.0.dist-info")).unwrap();
    fs::create_dir_all(python.join("fastapi")).unwrap();
    fs::write(npm.join("package.json"), r#"{"name":"vue","version":"3.5.44","types":"index.d.ts","engines":{"node":">=18"},"dependencies":{"@vue/runtime-dom":"3.5.44"}}"#).unwrap();
    fs::write(
        npm.join("README.md"),
        "# Vue\nInstalled Vue documentation\n",
    )
    .unwrap();
    fs::write(
        npm.join("index.d.ts"),
        "export declare function createApp(rootComponent: Component): App;\n",
    )
    .unwrap();
    fs::write(python.join("fastapi-0.162.0.dist-info/METADATA"), "Metadata-Version: 2.4\nName: fastapi\nVersion: 0.162.0\nRequires-Python: >=3.10\nRequires-Dist: starlette>=0.40\n\n# FastAPI\nInstalled documentation\n").unwrap();
    fs::write(
        python.join("fastapi-0.162.0.dist-info/RECORD"),
        "fastapi/__init__.py,,\nfastapi/applications.py,,\n",
    )
    .unwrap();
    fs::write(
        python.join("fastapi/__init__.py"),
        "from .applications import FastAPI\n",
    )
    .unwrap();
    fs::write(
        python.join("fastapi/applications.py"),
        format!("{}class FastAPI:\n    \"\"\"A web application.\"\"\"\n    def get(self, path: str): ...\n", "# import preamble\n".repeat(400)),
    )
    .unwrap();
    let selected = package_view(b"installed-package-routes", &[root.as_path()]);
    let catalog = [];
    let index = CatalogLookupIndex::from_catalog(&catalog);
    let graph = indexed_graph(Vec::new());
    let mut state = ProductState::open(root.join("product.json")).unwrap();
    for (purl, api, dependency) in [
        ("pkg:npm/vue@3.5.44", "createApp", "@vue/runtime-dom"),
        ("pkg:pypi/fastapi@0.162.0", "class FastAPI", "starlette"),
    ] {
        let package = PackageReference::parse(purl).unwrap();
        let run = |state: &mut ProductState, command| {
            state.execute(command, &selected, &catalog, &index, &graph, None)
        };
        let reply = run(
            &mut state,
            SurfaceCommand::Package {
                package: package.clone(),
            },
        )
        .unwrap();
        reply.admit(backend_library::CommandId::Package).unwrap();
        let SurfaceReply::PackageInstalled(facts) = &reply else {
            panic!("local installed evidence: {reply:?}")
        };
        assert_eq!(facts.package.as_str(), purl);
        assert!(
            facts
                .files
                .iter()
                .any(|file| file.excerpt.as_str().contains(api))
        );
        assert!(
            facts
                .requirements
                .iter()
                .any(|r| r.as_str().contains(dependency))
        );
        assert!(!facts.engines.is_empty());
        for file in facts.files.iter() {
            let source = fs::read(root.join(file.path.as_str())).unwrap();
            let start = file.offset as usize;
            assert_eq!(
                &source[start..start + file.excerpt.as_str().len()],
                file.excerpt.as_str().as_bytes()
            );
            if file.path.as_str().ends_with("applications.py") {
                assert!(
                    file.offset > 4096,
                    "native syntax locates API after a long import preamble"
                );
                assert!(file.excerpt.as_str().starts_with("class FastAPI"));
            }
        }

        let encoded = serde_json::to_vec(&reply).unwrap();
        let decoded: SurfaceReply = serde_json::from_slice(&encoded).unwrap();
        decoded.admit(backend_library::CommandId::Package).unwrap();
        let deps = run(
            &mut state,
            SurfaceCommand::Dependencies {
                package: package.clone(),
            },
        )
        .unwrap();
        deps.admit(backend_library::CommandId::Dependencies)
            .unwrap();
        assert!(matches!(deps, SurfaceReply::DependenciesInstalled(_)));
        let versions = run(&mut state, SurfaceCommand::PackageVersions { package }).unwrap();
        versions
            .admit(backend_library::CommandId::PackageVersions)
            .unwrap();
        let SurfaceReply::PackageVersionsInstalled(versions) = versions else {
            panic!("installed versions")
        };
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].package.as_str(), purl);
    }
    // Metadata freshness is observed anew, including after reopening product state.
    fs::write(
        npm.join("package.json"),
        r#"{"name":"vue","version":"3.5.45","engines":{"node":">=22"}}"#,
    )
    .unwrap();
    let mut state = ProductState::open(root.join("product.json")).unwrap();
    let old = PackageReference::parse("pkg:npm/vue@3.5.44").unwrap();
    assert!(
        state
            .execute(
                SurfaceCommand::Package { package: old },
                &selected,
                &catalog,
                &index,
                &graph,
                None
            )
            .is_err()
    );
    let new = PackageReference::parse("pkg:npm/vue@3.5.45").unwrap();
    let reply = state
        .execute(
            SurfaceCommand::Package { package: new },
            &selected,
            &catalog,
            &index,
            &graph,
            None,
        )
        .unwrap();
    let SurfaceReply::PackageInstalled(facts) = reply else {
        panic!("new installed metadata")
    };
    assert_eq!(facts.engines[0].as_str(), "node >=22");
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn installed_package_sources_refuse_symlinks_escapes_and_nonregular_files() {
    use std::os::unix::fs::symlink;
    let root = fixture("installed-package-confinement");
    let external = fixture("installed-package-outside");
    let package = root.join("node_modules/vue");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        r#"{"name":"vue","version":"3.5.44","types":"../../outside.d.ts","main":"pipe.js"}"#,
    )
    .unwrap();
    fs::write(external.join("README.md"), "must not escape").unwrap();
    symlink(external.join("README.md"), package.join("README.md")).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        package.join("pipe.js"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();
    let selected = package_view(b"installed-package-confinement", &[root.as_path()]);
    let requested = PackageReference::parse("pkg:npm/vue@3.5.44").unwrap();
    let rows = super::super::installed_packages::find(&selected, &requested, false).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].files.len(),
        1,
        "only the regular confined metadata file"
    );
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(external).unwrap();
}
