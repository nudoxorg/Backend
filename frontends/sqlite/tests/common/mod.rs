//! Shared helpers: a compact textual rendering of an outline so tests can compare exactly
//! what was written, with every extent shown as the source text it covers.
#![allow(dead_code)]
use backend_sql_contract::{
    DefinitionAction, Extent, Outline, Request, Response, Settings, StatementOutline,
    StatementSupport, Syntax,
};

pub fn request(source: &str) -> Request {
    Request {
        version: backend_sql_contract::PROTOCOL_VERSION,
        expected_basis: None,
        source: source.as_bytes().to_vec(),
        settings: Settings::ProcessDefaults,
        context: None,
    }
}

pub fn analyze(source: &str) -> Response {
    backend_frontend_sqlite_experimental::analyze_native(&request(source))
}

/// Parse in-process, require a validated outline, and return it.
pub fn outline(source: &str) -> Outline {
    let response = analyze(source);
    assert!(
        matches!(response.syntax, Syntax::Parsed { .. }),
        "not parsed: {:?}",
        response.syntax
    );
    let outline = response.outline.expect("a parsed file carries an outline");
    assert!(outline.validate(source.as_bytes()), "outline must validate");
    outline
}

pub fn text<'s>(source: &'s str, e: &Extent) -> &'s str {
    &source[e.start..e.end]
}

fn opt(prefix: &str, v: &Option<String>) -> String {
    v.as_ref().map(|v| format!("{prefix}{v}")).unwrap_or_default()
}

/// One line per item; every extent is printed as the source text it covers.
pub fn render_statement(src: &str, s: &StatementOutline) -> String {
    let mut out = format!("{:?} {:?}\n", s.kind, s.support);
    for d in &s.definitions {
        let action = match &d.action {
            DefinitionAction::Rename { to, to_extent } => {
                format!("Rename({to}@{:?})", text(src, to_extent))
            }
            other => format!("{other:?}"),
        };
        out.push_str(&format!(
            "  def {:?} {} {}{}{}{}{} name@{:?} in@{:?}{}\n",
            d.kind,
            action,
            if d.conditional { "?" } else { "" },
            opt("schema=", &d.schema),
            d.name,
            opt(" parent=", &d.parent),
            opt(" parent_schema=", &d.parent_schema),
            text(src, &d.name_extent),
            text(src, &d.extent),
            d.detail
                .as_ref()
                .map(|v| format!(" detail={v:?}"))
                .unwrap_or_default(),
        ));
    }
    for r in &s.references {
        out.push_str(&format!(
            "  ref {:?}/{:?} {}{}{} @{:?} full@{:?}\n",
            r.kind,
            r.role,
            opt("", &r.schema).replace(|_| false, "") + if r.schema.is_some() { "." } else { "" },
            opt("", &r.qualifier) + if r.qualifier.is_some() { "." } else { "" },
            r.name,
            text(src, &r.extent),
            text(src, &r.full_extent),
        ));
    }
    for e in &s.scope {
        out.push_str(&format!(
            "  scope {:?} {} rel={}{} cols={:?} in@{:?}\n",
            e.kind,
            e.visible_as,
            e.relation.as_deref().unwrap_or("-"),
            e.relation_schema
                .as_ref()
                .map(|v| format!(" schema={v}"))
                .unwrap_or_default(),
            e.columns,
            text(src, &e.region),
        ));
    }
    out
}

pub fn render(src: &str, outline: &Outline) -> String {
    outline
        .statements
        .iter()
        .map(|s| render_statement(src, s))
        .collect::<Vec<_>>()
        .join("")
}

pub fn supports(outline: &Outline) -> Vec<StatementSupport> {
    outline.statements.iter().map(|s| s.support).collect()
}
