use compiler_languages_java::purl::{MavenCoordinates, PurlError};

#[test]
fn parses_both_canonical_maven_spellings_without_copying_coordinates() {
    let input = "maven:org.example:tool@1.2+-3";
    let coordinates = MavenCoordinates::parse(input).expect("valid PURL");
    assert_eq!(coordinates.group, &input[6..17]);
    assert_eq!(coordinates.artifact, &input[18..22]);
    assert_eq!(coordinates.version, &input[23..]);

    let package = MavenCoordinates::parse("pkg:maven/a/b@v").expect("valid package URL");
    assert_eq!(
        (package.group, package.artifact, package.version),
        ("a", "b", "v")
    );
}

#[test]
fn rejects_empty_and_illegal_segments_with_input_and_position() {
    let cases = [
        ("maven::a@v", 6),
        ("maven:a:@v", 8),
        ("maven:.a:b@v", 6),
        ("maven:a/b:c@v", 7),
        ("maven:a:a@", 10),
        ("maven:a:a@v@x", 11),
        ("maven:a:a@v x", 11),
    ];
    for (input, position) in cases {
        let error = MavenCoordinates::parse(input).expect_err("hostile PURL accepted");
        assert_eq!(
            error,
            match input {
                "maven:a:a@" | "maven:a:a@v@x" | "maven:a:a@v x" =>
                    PurlError::MalformedVersion { input, position },
                _ => PurlError::MalformedSegment { input, position },
            }
        );
    }
}

#[test]
fn rejects_unsupported_scheme_and_missing_version() {
    let input = "npm:a:b@v";
    assert_eq!(
        MavenCoordinates::parse(input),
        Err(PurlError::UnsupportedScheme { input, position: 0 })
    );
    let input = "maven:a:b";
    assert_eq!(
        MavenCoordinates::parse(input),
        Err(PurlError::MalformedVersion {
            input,
            position: input.len()
        })
    );
}
