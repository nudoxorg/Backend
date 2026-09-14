use backend_frontend_java::legacy::{central::Central, purl::MavenCoordinates};

#[test]
fn central_urls_use_maven_layout_for_both_artifacts() {
    let central = Central::new("https://example.test/maven2/");
    let coordinates = MavenCoordinates::parse("maven:org.example:demo@1.2.3").unwrap();
    assert_eq!(
        central.jar_url(&coordinates),
        "https://example.test/maven2/org/example/demo/1.2.3/demo-1.2.3.jar"
    );
    assert_eq!(
        central.sources_jar_url(&coordinates),
        "https://example.test/maven2/org/example/demo/1.2.3/demo-1.2.3-sources.jar"
    );
}
