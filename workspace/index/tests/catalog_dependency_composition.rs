//! One published coordinate's dependency names, four manifests, one sweep.
//!
//! Maven, Go, PyPI, and crates.io each reduce a document to sorted names.
//! Those names are the edges [`count_dependents`] counts. A name that one
//! parser drops must stay dropped in the sweep.

use index::{
    ecosystem::{Language, pom_dependency_names, require_names, requires_dist_names},
    record::{PackageRecord, edge_names_agree, runtime_edges_from_names},
    search::ranking::dependents::{DependencyRow, count_dependents},
    upstream::{crates_catalog::normal_dependency_names, maven_search::pom_url},
};
use smol_str::SmolStr;

fn row(ecosystem: Language, name: &str, dependencies: &[String]) -> DependencyRow {
    DependencyRow {
        ecosystem,
        name: SmolStr::new(name),
        dependencies: dependencies.iter().map(SmolStr::new).collect(),
    }
}

#[test]
fn four_manifests_agree_on_one_dependents_sweep() {
    let pom = br#"<project>
      <dependencies>
        <dependency><groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId></dependency>
        <dependency><groupId>junit</groupId><artifactId>junit</artifactId><scope>test</scope></dependency>
      </dependencies>
      <dependencyManagement><dependencies>
        <dependency><groupId>com.managed</groupId><artifactId>bom</artifactId></dependency>
      </dependencies></dependencyManagement>
    </project>"#;
    let go_mod = "module example.com/app\n\nrequire (\n\trsc.io/quote v1.5.2\n\tgolang.org/x/text v0.3.0 // indirect\n)\n";
    let pypi = br#"{"info":{"requires_dist":["Requests>=2.0", null, "Foo.Bar"]}}"#;
    let crates = br#"{"dependencies":[
        {"crate_id":"serde","kind":"normal"},
        {"crate_id":"tokio","kind":"dev"},
        {"crate_id":"serde","kind":"normal"}
    ]}"#;

    let java = pom_dependency_names(pom);
    let go = require_names(go_mod);
    let python = requires_dist_names(pypi);
    let rust = normal_dependency_names(crates);

    assert_eq!(java, vec!["org.slf4j:slf4j-api".to_owned()]);
    assert_eq!(go, vec![
        "golang.org/x/text".to_owned(),
        "rsc.io/quote".to_owned()
    ]);
    assert_eq!(python, vec!["foo-bar".to_owned(), "requests".to_owned()]);
    assert_eq!(rust, vec!["serde".to_owned()]);

    let counts = count_dependents([
        row(Language::Java, "com.example:app", &java),
        row(Language::Go, "example.com/app", &go),
        row(Language::Python, "app", &python),
        row(Language::Rust, "app", &rust),
        row(Language::Java, "org.other:lib", &java),
    ]);

    assert_eq!(
        counts[&(Language::Java, SmolStr::new("org.slf4j:slf4j-api"))],
        2
    );
    assert_eq!(counts[&(Language::Go, SmolStr::new("rsc.io/quote"))], 1);
    assert_eq!(counts[&(Language::Python, SmolStr::new("requests"))], 1);
    assert_eq!(counts[&(Language::Rust, SmolStr::new("serde"))], 1);
    assert!(!counts.contains_key(&(Language::Java, SmolStr::new("junit:junit"))));
    assert!(!counts.contains_key(&(Language::Rust, SmolStr::new("tokio"))));
    assert_eq!(
        pom_url("org.slf4j:slf4j-api", "2.0.9").as_deref(),
        Some("https://repo1.maven.org/maven2/org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.pom")
    );

    let published = PackageRecord::from_parts(
        Language::Java,
        "com.example:app",
        "1.0.0",
        None,
        None,
        Vec::new(),
        None,
        None,
        false,
        runtime_edges_from_names(&["  org.slf4j:slf4j-api ", "org.slf4j:slf4j-api", " "]),
    );
    assert!(edge_names_agree(
        &published,
        &["org.slf4j:slf4j-api"]
    ));
    assert_eq!(published.edges.len(), 1);
}
