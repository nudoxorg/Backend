//! Native grammar controls for written Python headers. These are syntax facts,
//! not resolved Python types or cross-file semantic authority.
use backend_compile::{SourceAnalysis, SourceDeclaration, SyntaxError};
use std::path::Path;

fn declaration<'a>(analysis: &'a SourceAnalysis, name: &str) -> &'a SourceDeclaration {
    analysis
        .declarations()
        .iter()
        .find(|row| row.name() == name)
        .expect("captured declaration")
}

#[test]
fn audited_actual_apps_keep_headers_separate_from_decorators_and_bodies() -> Result<(), SyntaxError>
{
    let frontend = backend_frontend_python::syntax_frontend()?;
    let docs = frontend.analyze(
        Path::new("docs-utils.py"),
        include_bytes!("fixtures/source-signatures/docs-utils.py"),
    )?;
    let docs_function = declaration(&docs, "generate_s3_authorization_headers");
    assert_eq!(docs_function.line(), 70);
    assert_eq!(
        docs_function.signature(),
        "def generate_s3_authorization_headers(key):"
    );
    assert!(
        docs_function
            .documentation()
            .contains("Generate authorization headers for an s3 object.")
    );
    assert!(
        docs_function
            .source_excerpt()
            .text()
            .expect("written source")
            .contains("Params={\"Bucket\"")
    );

    let open_webui = frontend.analyze(
        Path::new("open-webui-openai.py"),
        include_bytes!("fixtures/source-signatures/open-webui-openai.py"),
    )?;
    let function = declaration(&open_webui, "delete_provider_model");
    assert_eq!(function.line(), 1031);
    assert_eq!(
        function.signature(),
        "async def delete_provider_model(\n    request: Request,\n    url_idx: int,\n    model: str,\n    user=Depends(get_admin_user),\n):"
    );
    assert!(
        function
            .source_excerpt()
            .text()
            .expect("written source")
            .contains("query={'model': actual_model}")
    );
    assert!(!function.signature().contains("router.delete"));
    Ok(())
}

#[test]
fn python_headers_follow_suite_tokens_through_nested_defaults_annotations_and_async()
-> Result<(), SyntaxError> {
    let source = r###"@router.delete('/models/{url_idx}')
@decorate(lambda value: {"brace": "}"})
async def inspect_model(
    name: "Literal['{', ':']",
    settings: dict[str, object] = {"nested": {"text": "}"}},
    callback=lambda value: {"item": value},
) -> tuple[
    dict[str, str],
    "Literal['{']",
]:
    """Body docs with { and : must remain separate."""
    return {"body": "not a signature"}

class Container(dict[str, "Literal['{']"]):
    label: str = "{value}"
    def inline(self, value={"x": "}"}): return {"body": value}
"###;
    let frontend = backend_frontend_python::syntax_frontend()?;
    let analysis = frontend.analyze(Path::new("headers.py"), source.as_bytes())?;
    let function = declaration(&analysis, "inspect_model");
    assert_eq!(
        function.signature(),
        "async def inspect_model(\n    name: \"Literal['{', ':']\",\n    settings: dict[str, object] = {\"nested\": {\"text\": \"}\"}},\n    callback=lambda value: {\"item\": value},\n) -> tuple[\n    dict[str, str],\n    \"Literal['{']\",\n]:"
    );
    assert_eq!(
        function.documentation(),
        "Body docs with { and : must remain separate."
    );
    assert_eq!(
        declaration(&analysis, "Container").signature(),
        "class Container(dict[str, \"Literal['{']\"]):"
    );
    assert_eq!(
        declaration(&analysis, "inline").signature(),
        "def inline(self, value={\"x\": \"}\"}):"
    );
    assert_eq!(
        declaration(&analysis, "label").signature(),
        "label: str = \"{value}\""
    );
    Ok(())
}
