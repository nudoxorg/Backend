//! Guards the shipping Java image writer against lexical occurrence recovery.
//! Calls must arise from `JavacTask` tree paths resolved through `Trees.getElement`.
//! The JDK integration test independently proves the resulting overload facts.

const IMAGE: &str = include_str!("../src/legacy/doclet/AuthorityImage.java");
const COMPILER_EXTRACTOR: &str = include_str!("../src/legacy/doclet/CompilerExtractor.java");

#[derive(Debug, thiserror::Error)]
enum AuthorityFailure {
    #[error("the Java doclet lacks required authority operation `{operation}`")]
    Missing { operation: &'static str },
    #[error("the Java doclet retains forbidden lexical occurrence machinery `{operation}`")]
    LexicalFallback { operation: &'static str },
}

fn required(operation: &'static str) -> Result<(), AuthorityFailure> {
    if IMAGE.contains(operation) {
        Ok(())
    } else {
        Err(AuthorityFailure::Missing { operation })
    }
}

fn forbidden(operation: &'static str) -> Result<(), AuthorityFailure> {
    if IMAGE.contains(operation) {
        Err(AuthorityFailure::LexicalFallback { operation })
    } else {
        Ok(())
    }
}

#[test]
fn shipping_image_calls_are_walked_and_resolved_by_javac_trees() -> Result<(), AuthorityFailure> {
    for operation in [
        "TreePathScanner",
        "visitMethodInvocation",
        "trees.getElement(",
        "trees.getSourcePositions()",
    ] {
        required(operation)?;
    }
    for operation in [
        "java.util.regex",
        "Pattern.compile",
        "Matcher",
        "matchingBrace",
        "Files.readString",
        "getCharContent(true)",
    ] {
        forbidden(operation)?;
    }
    Ok(())
}

#[test]
fn compiler_entrypoint_binds_the_profile_before_attribution() -> Result<(), AuthorityFailure> {
    for operation in [
        "JavacTask",
        "task.parse()",
        "task.analyze()",
        "--release",
        "throwIfCompilationFailed",
        "AuthorityImage.write",
    ] {
        if COMPILER_EXTRACTOR.contains(operation) {
            continue;
        }
        return Err(AuthorityFailure::Missing { operation });
    }
    if COMPILER_EXTRACTOR.contains("new Extractor") {
        return Err(AuthorityFailure::LexicalFallback {
            operation: "parallel JSON authority product",
        });
    }
    Ok(())
}
