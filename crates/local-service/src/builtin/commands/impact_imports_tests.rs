//! The component graph must follow imports exactly as the compiler resolves
//! them, and say so when it cannot.
use super::*;

const TSCONFIG: &str = r#"{"compilerOptions":{"jsx":"preserve","paths":{"@/*":["./src/*"]}}}"#;

fn sources() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "src/components/Button.tsx",
            "export function Button() { return <button />; }",
        ),
        (
            "src/components/Card.tsx",
            "export default function Card() { return <div />; }",
        ),
        (
            "src/components/NavBar.tsx",
            r#"import { Button } from "./Button";
export function NavBar() { return <nav><Button /></nav>; }"#,
        ),
        (
            "src/components/index.ts",
            r#"export { Button } from "./Button";
export { NavBar } from "./NavBar";
export { default as Card } from "./Card";"#,
        ),
        (
            "src/app/page.tsx",
            r#"import { Button } from "@/components/Button";
import { NavBar } from "@/components/NavBar";
export default function Home() { return <main><NavBar /><Button /></main>; }"#,
        ),
        (
            "src/app/about/page.tsx",
            r#"import { Button } from "../../components/Button";
export default function About() { return <Button />; }"#,
        ),
        (
            "src/app/barrel/page.tsx",
            r#"import { Button as Primary, Card } from "@/components";
export default function Barrel() { return <div><Primary /><Card /></div>; }"#,
        ),
        (
            "src/app/broken/page.tsx",
            r#"import { Ghost } from "@/widgets/Ghost";
import React from "react";
import logo from "@/assets/logo.svg";
export default function Broken() { return <Ghost />; }"#,
        ),
    ]
}

fn scans() -> BTreeMap<String, FileScan> {
    sources()
        .into_iter()
        .map(|(path, text)| {
            let profile = profile_for(path).expect("profile");
            let scan = with_analysis(profile, text, |module| scan_module(path, text, &module))
                .expect("parse");
            (path.to_owned(), scan)
        })
        .collect()
}

fn renderers_of(
    nodes: &BTreeMap<Key, Node>,
    edges: &[Edge],
    file: &str,
    name: &str,
) -> BTreeSet<(String, String)> {
    let target = component_key(file, name);
    edges
        .iter()
        .filter(|edge| edge.relation == "code.renders" && edge.from == target)
        .map(|edge| {
            let node = &nodes[&edge.to];
            (node.path.clone().unwrap_or_default(), node.name.clone())
        })
        .collect()
}

#[test]
fn consumers_through_tsconfig_paths_are_found_like_relative_ones() {
    let scans = scans();
    let indexed: BTreeSet<String> = scans.keys().cloned().collect();
    let read = |path: &str| Ok((path == "tsconfig.json").then(|| TSCONFIG.as_bytes().to_vec()));
    let imports = CompilerImports::new(&indexed, &read);
    let (nodes, edges, gaps) = build_graph_with(&scans, &imports);

    let want: BTreeSet<(String, String)> = [
        ("src/components/NavBar.tsx", "NavBar"),
        ("src/app/page.tsx", "Home"),
        ("src/app/about/page.tsx", "About"),
        ("src/app/barrel/page.tsx", "Barrel"),
    ]
    .into_iter()
    .map(|(file, name)| (file.to_owned(), name.to_owned()))
    .collect();
    assert_eq!(
        renderers_of(&nodes, &edges, "src/components/Button.tsx", "Button"),
        want
    );
    // The barrel aliased through `@/components` reaches the default export too.
    assert!(
        renderers_of(&nodes, &edges, "src/components/Card.tsx", "Card")
            .contains(&("src/app/barrel/page.tsx".to_owned(), "Barrel".to_owned()))
    );
    assert!(
        renderers_of(&nodes, &edges, "src/components/NavBar.tsx", "NavBar")
            .contains(&("src/app/page.tsx".to_owned(), "Home".to_owned()))
    );

    // The one alias no tsconfig entry satisfies is reported, first, with its reason;
    // packages and assets are not.
    let gap = gaps
        .iter()
        .find(|gap| gap.facet == "code.unresolved-imports")
        .expect("unresolved import gap");
    assert_eq!(gaps[0].facet, "code.unresolved-imports");
    assert_eq!(gap.count, Some(1));
    assert!(
        gap.note.contains("src/app/broken/page.tsx") && gap.note.contains("@/widgets/Ghost"),
        "{}",
        gap.note
    );
    assert!(
        gap.note.contains("\"@/*\""),
        "the reason names the paths entry: {}",
        gap.note
    );
    assert!(
        !gap.note.contains("react") && !gap.note.contains("logo.svg"),
        "{}",
        gap.note
    );
    assert!(
        gaps.iter()
            .any(|gap| gap.facet == "code.unresolved-renders"),
        "the Ghost tag stays an unresolved render"
    );
}

#[test]
fn relative_only_lookup_misses_alias_consumers_and_says_so() {
    // Without the project configuration (the old relative-only lookup) the alias
    // consumers cannot be placed, and every one of them is reported.
    let scans = scans();
    let indexed: BTreeSet<String> = scans.keys().cloned().collect();
    let (nodes, edges, gaps) = build_graph_with(&scans, &RelativeImports(&indexed));
    let found = renderers_of(&nodes, &edges, "src/components/Button.tsx", "Button");
    assert!(!found.contains(&("src/app/page.tsx".to_owned(), "Home".to_owned())));
    let gap = gaps
        .iter()
        .find(|gap| gap.facet == "code.unresolved-imports")
        .expect("gap");
    assert!(gap.count.unwrap_or(0) >= 4, "{gap:?}");
}

#[test]
fn a_tsconfig_that_cannot_be_read_is_a_named_failure() {
    let scans = scans();
    let indexed: BTreeSet<String> = scans.keys().cloned().collect();
    let read = |path: &str| {
        if path == "tsconfig.json" {
            Err(std::io::Error::other("denied"))
        } else {
            Ok(None)
        }
    };
    let imports = CompilerImports::new(&indexed, &read);
    let (_, _, gaps) = build_graph_with(&scans, &imports);
    let gap = gaps
        .iter()
        .find(|gap| gap.facet == "code.unresolved-imports")
        .expect("gap");
    assert!(
        gap.count.unwrap_or(0) >= 4 && gap.note.contains("denied"),
        "{gap:?}"
    );
    assert_eq!(imports.configuration_errors().len(), 1);
}
