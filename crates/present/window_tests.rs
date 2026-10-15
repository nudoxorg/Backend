//! Windows over complete lists: members, source lines, product rows, outline
//! entries, and the neighbourhood of one declaration.
//!
//! These assert the one policy every list-bearing answer shares: no read
//! fails because of its size, every row is reachable by cursor exactly once,
//! the closing line names the exact next call, and a cursor taken from a list
//! that has since changed is refused rather than silently skipping rows.

#![allow(clippy::expect_used, clippy::panic)]

use crate::listing::{window_members, window_source};
use crate::window::{WindowCursor, WindowKind, WindowRequest};
use crate::{
    Answer, ContinuationCursor, ContinuationTarget, Detail, FaultSlug, OutlineEntry, OutlineTree,
    Page, PageDto, PageProjection, ProductView, markdown, page_from_document, text,
};
use backend_library::{
    Basis, DeclarationKind, Document, Fragment, OutlineExtent, OutlineNode, Row, RowId,
    SourceAvailability, SourceExcerpt, SourceExcerptExtent, SourceLocation, package_key,
    symbol_key, view_state_root,
};

const PROJECT: &str = "/abs/polyglot";
const DECLARATION: &str = "/abs/polyglot::src/color.go:71::Color";

fn basis() -> Basis {
    Basis::new(
        view_state_root(&[]),
        backend_library::object_version(b"window-tests"),
    )
}

/// One type with `count` methods, each with a signature and a documentation
/// sentence, as one owner document plus the rows around it.
fn type_with_members(count: usize) -> Page {
    let centre = symbol_key(DECLARATION);
    let mut rows = vec![
        Row::in_package(RowId::Symbol(centre), basis(), package_key(PROJECT), DECLARATION)
            .with_kind(DeclarationKind::Struct)
            .with_source(SourceLocation::new("src/color.go", 71).expect("site")),
    ];
    for index in 0..count {
        let label = format!("{PROJECT}::src/color.go:{}::Color::Method{index:03}", 100 + index);
        rows.push(
            Row::in_package(
                RowId::Symbol(symbol_key(&label)),
                basis(),
                package_key(PROJECT),
                &label,
            )
            .with_parent(centre)
            .with_kind(DeclarationKind::Method)
            .with_signature(format!(
                "func (c *Color) Method{index:03}(w io.Writer, format string, a ...interface{{}}) (n int, err error)"
            ))
            .with_source(SourceLocation::new("src/color.go", 100 + index as u32).expect("site")),
        );
    }
    let document = Document::new(
        centre,
        basis().root,
        [Fragment::Text("Color defines a custom color object.".to_owned())],
    )
    .with_location(SourceAvailability::Captured(
        SourceLocation::new("src/color.go", 71).expect("site"),
    ))
    .with_excerpt(
        SourceExcerpt::captured("type Color struct {\n\tparams []Attribute\n}", SourceExcerptExtent::Complete)
            .expect("excerpt"),
    );
    page_from_document(DECLARATION, &document, &rows, &[], Vec::new())
}

fn member_names(page: &Page) -> Vec<String> {
    page.members()
        .iter()
        .flat_map(|group| group.members())
        .map(|member| member.identity().name().to_owned())
        .collect()
}

fn mcp_arguments() -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("coordinate".to_owned(), DECLARATION.into());
    arguments
}

#[test]
fn a_type_with_many_members_pages_every_member_exactly_once() {
    let page = type_with_members(150);
    let mut seen = Vec::new();
    let mut request = WindowRequest::first();
    let mut windows = 0;
    loop {
        let window = window_members(page.clone(), request).expect("window");
        let answer = Answer::Page(Box::new(window.clone()));
        let cursor = answer.cursor();
        let token = match cursor {
            Some(ContinuationCursor::Window(cursor)) => Some(cursor.encode()),
            Some(other) => panic!("a member window issues a window cursor, not {other:?}"),
            None => None,
        };
        let arguments = mcp_arguments();
        let rendered = markdown::answer_with_detail(
            &answer,
            Detail::Standard,
            token.as_deref(),
            ContinuationTarget::McpCall {
                tool: "backend.document",
                arguments: &arguments,
            },
        );
        assert!(
            rendered.len() <= 9 * 1024,
            "a page of members stays inside its text budget: {} bytes",
            rendered.len()
        );
        let typed = crate::encode_answer(
            &answer,
            Detail::Standard,
            token.as_deref(),
            crate::DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("a window always fits the typed budget");
        assert!(typed.bytes.len() <= 36 * 1024, "{} bytes", typed.bytes.len());
        seen.extend(member_names(&window));
        windows += 1;
        match token {
            Some(token) => {
                assert!(
                    rendered.contains(&format!("\"cursor\":\"{token}\"")),
                    "the exact next call carries its cursor in the text: {rendered}"
                );
                assert!(rendered.contains("more members; next page: backend.document"));
                request = WindowRequest::first()
                    .with_cursor(WindowCursor::decode(&token).expect("our own cursor"));
            }
            None => {
                assert!(!rendered.contains("next page"), "{rendered}");
                break;
            }
        }
        assert!(windows < 10, "paging terminates");
    }
    assert!(windows >= 2, "150 members do not fit one window");
    let expected = (0..150).map(|index| format!("Method{index:03}")).collect::<Vec<_>>();
    assert_eq!(seen, expected, "every member once, in order");
}

#[test]
fn the_first_window_carries_the_page_and_later_windows_only_members() {
    let page = type_with_members(150);
    let first = window_members(page.clone(), WindowRequest::first()).expect("first");
    let rendered = markdown::page_projection(&PageProjection::new(&first, Detail::Standard));
    assert!(rendered.contains("Color defines a custom color object."));
    assert!(rendered.contains("```go"));
    assert!(rendered.contains("## source"));
    let cursor = first
        .listing()
        .and_then(|listing| listing.window().next())
        .expect("rows remain");
    let second = window_members(page, WindowRequest::first().with_cursor(cursor)).expect("second");
    let rendered = markdown::page_projection(&PageProjection::new(&second, Detail::Standard));
    assert!(!rendered.contains("Color defines a custom color object."), "{rendered}");
    assert!(!rendered.contains("## source"), "{rendered}");
    assert!(rendered.contains("## members"), "{rendered}");
    let typed = PageDto::from_projection(&PageProjection::new(&second, Detail::Standard));
    assert!(typed.prose.is_empty() && typed.source.is_none() && typed.tokens.is_empty());
    let window = typed.window.expect("a cut list reports its window");
    assert_eq!((window.list.as_str(), window.total), ("members", 150));
    assert!(window.offset > 0);
}

#[test]
fn a_summary_still_names_every_member_it_carries() {
    let page = window_members(type_with_members(12), WindowRequest::first()).expect("window");
    let rendered = markdown::page_projection(&PageProjection::new(&page, Detail::Summary));
    assert!(rendered.contains("Method000, Method001"), "{rendered}");
    assert!(rendered.contains("Method011"), "{rendered}");
    assert!(!rendered.contains("io.Writer"), "a summary names, it does not spell signatures");
    let standard = markdown::page_projection(&PageProjection::new(&page, Detail::Standard));
    assert!(standard.contains("io.Writer"), "{standard}");
    // One compact line per member: a handle that names it, then its signature.
    assert!(standard.contains("src/color.go:100::Color::Method000  func (c *Color) Method000("));
}

#[test]
fn a_cursor_from_a_changed_member_list_is_refused_not_followed() {
    let page = type_with_members(150);
    let first = window_members(page, WindowRequest::first()).expect("first");
    let cursor = first
        .listing()
        .and_then(|listing| listing.window().next())
        .expect("rows remain");
    let changed = type_with_members(151);
    let fault = window_members(changed, WindowRequest::first().with_cursor(cursor))
        .expect_err("the list changed under the cursor");
    assert_eq!(fault.slug(), FaultSlug::CursorMismatch);
}

#[test]
fn an_explicit_limit_bounds_the_window_and_a_small_list_is_one_window() {
    let page = type_with_members(40);
    let asked = window_members(page.clone(), WindowRequest::first().with_limit(5)).expect("window");
    assert_eq!(member_names(&asked).len(), 5);
    let window = asked.listing().expect("listing").window();
    assert_eq!((window.shown(), window.remaining()), (5, 35));
    let small = window_members(type_with_members(8), WindowRequest::first()).expect("window");
    let window = small.listing().expect("listing").window();
    assert!(!window.is_cut(), "every member fits: nothing is cut");
    let typed = PageDto::from_projection(&PageProjection::new(&small, Detail::Standard));
    assert!(typed.window.is_none(), "an uncut list reports no window");
    assert!(typed.members.iter().all(|group| group.total.is_none()));
}

#[test]
fn source_pages_by_line_when_bounded_and_every_line_is_reachable() {
    let centre = symbol_key(DECLARATION);
    // An owner retains at most 4,096 bytes of a declaration's source; a
    // caller who bounds a page still reaches every retained line.
    let body = (0..150)
        .map(|line| format!("    v{line:03} := f({line:03})"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(body.len() <= 4_096);
    let document = Document::new(centre, basis().root, [Fragment::Text("docs".to_owned())])
        .with_location(SourceAvailability::Captured(
            SourceLocation::new("src/long.go", 10).expect("site"),
        ))
        .with_excerpt(SourceExcerpt::captured(&body, SourceExcerptExtent::Complete).expect("excerpt"));
    let page = page_from_document(DECLARATION, &document, &[], &[], Vec::new());
    // Unbounded, the whole retained excerpt is one window: a `source` answer
    // is the code, never cut by the document's own source budget.
    let whole = window_source(page.clone(), WindowRequest::first()).expect("whole");
    let rendered = markdown::answer_with_detail(
        &Answer::Page(Box::new(whole)),
        Detail::Standard,
        None,
        ContinuationTarget::McpTool("backend.source"),
    );
    assert_eq!(rendered.matches(":= f(").count(), 150, "{rendered}");
    assert!(!rendered.contains("next page"), "{rendered}");

    let mut lines = 0;
    let mut pages = 0;
    let mut request = WindowRequest::first().with_limit(40);
    loop {
        let window = window_source(page.clone(), request).expect("window");
        let answer = Answer::Page(Box::new(window));
        let cursor = match answer.cursor() {
            Some(ContinuationCursor::Window(cursor)) => Some(cursor),
            _ => None,
        };
        let token = cursor.map(WindowCursor::encode);
        let rendered = markdown::answer_with_detail(
            &answer,
            Detail::Standard,
            token.as_deref(),
            ContinuationTarget::McpTool("backend.source"),
        );
        lines += rendered.matches(":= f(").count();
        pages += 1;
        match cursor {
            Some(cursor) => {
                assert!(rendered.contains("more source lines; next page:"), "{rendered}");
                request = WindowRequest::first().with_limit(40).with_cursor(cursor);
            }
            None => break,
        }
        assert!(pages < 20);
    }
    assert_eq!((lines, pages), (150, 4), "every retained line is reachable");
}

#[test]
fn a_standard_document_cuts_long_documentation_and_source_and_says_so() {
    let centre = symbol_key(DECLARATION);
    let docs = (0..30)
        .map(|paragraph| Fragment::Text(format!("paragraph {paragraph}: {}\n\n", "documentation ".repeat(30))))
        .collect::<Vec<_>>();
    let body = (0..400).map(|line| format!("line {line}")).collect::<Vec<_>>().join("\n");
    let document = Document::new(centre, basis().root, docs)
        .with_location(SourceAvailability::Captured(
            SourceLocation::new("src/long.go", 10).expect("site"),
        ))
        .with_excerpt(SourceExcerpt::captured(&body, SourceExcerptExtent::Complete).expect("excerpt"));
    let page = page_from_document(DECLARATION, &document, &[], &[], Vec::new());
    let standard = markdown::page_projection(&PageProjection::new(&page, Detail::Standard));
    assert!(standard.len() < 7 * 1024, "{} bytes", standard.len());
    assert!(standard.contains("more byte(s) of documentation; request detail=full"), "{standard}");
    assert!(standard.contains("more source line(s); read them all with `source`"), "{standard}");
    let typed = PageDto::from_projection(&PageProjection::new(&page, Detail::Standard));
    assert!(typed.prose_omitted_bytes.is_some_and(|omitted| omitted > 0));
    let source = typed.source.expect("source");
    assert_eq!(source.extent, "truncated");
    assert!(source.omitted_lines.is_some_and(|omitted| omitted > 0));
    let full = markdown::page_projection(&PageProjection::new(&page, Detail::Full));
    assert!(full.len() > standard.len(), "full carries more than standard");
    assert!(full.len() < 40 * 1024, "{} bytes", full.len());
    let ladder = crate::encode_answer_fitting(
        &Answer::Page(Box::new(page)),
        Detail::Full,
        None,
        crate::DEFAULT_RESPONSE_BUDGET_BYTES,
    )
    .expect("a page always encodes at some detail");
    assert!(ladder.0.bytes.len() <= crate::DEFAULT_RESPONSE_BUDGET_BYTES);
}

fn reference_view(count: usize) -> ProductView {
    let rows = (0..count)
        .map(|index| {
            crate::ProductRecord::new(
                format!("src/caller_{index:03}.go:{}::Caller{index:03}", index + 1),
                Some(format!("{PROJECT}::src/caller_{index:03}.go:{}::Caller{index:03}", index + 1)),
                vec!["calls".to_owned(), "indexed".to_owned()],
            )
            .compact()
        })
        .collect::<Vec<_>>();
    ProductView::assembled("references to Color", rows)
}

#[test]
fn a_long_reference_list_pages_by_cursor_with_the_call_in_the_text() {
    let view = reference_view(180);
    let mut seen = 0;
    let mut request = WindowRequest::first();
    loop {
        let window = view.clone().windowed(request).expect("window");
        let answer = Answer::Product(Box::new(window));
        let cursor = match answer.cursor() {
            Some(ContinuationCursor::Window(cursor)) => Some(cursor),
            _ => None,
        };
        let token = cursor.map(WindowCursor::encode);
        let mut arguments = serde_json::Map::new();
        arguments.insert("coordinate".to_owned(), "Color".into());
        let rendered = markdown::answer_with_detail(
            &answer,
            Detail::Standard,
            token.as_deref(),
            ContinuationTarget::McpCall {
                tool: "backend.references",
                arguments: &arguments,
            },
        );
        assert!(rendered.len() <= 9 * 1024, "{} bytes", rendered.len());
        seen += rendered.lines().filter(|line| line.contains("Caller")).count();
        match cursor {
            Some(cursor) => {
                assert!(rendered.contains("more rows; next page: backend.references"), "{rendered}");
                request = WindowRequest::first().with_cursor(cursor);
            }
            None => break,
        }
    }
    assert_eq!(seen, 180);
}

#[test]
fn a_reference_row_is_one_line_with_no_repeated_coordinate() {
    let reply = backend_library::SurfaceReply::References {
        target: backend_library::ProductText::new("pkg::semantic::callee").expect("target"),
        references: Box::new([backend_library::ReferenceRecord {
            site: backend_library::ProductText::new("/p::src/main.go:7::Run").expect("site"),
            target: backend_library::SemanticLinkTarget::Local {
                declaration: backend_library::SemanticDeclarationIdentity {
                    family: [1; 16],
                    variant: [2; 16],
                },
            },
            relation: backend_library::SemanticLinkKind::Calls,
            evidence: backend_library::SemanticLinkEvidence {
                confidence: backend_library::SemanticConfidence::Indexed,
                source: None,
            },
        }]),
    };
    let view = crate::product_view(&reply);
    let rendered = markdown::product(&view);
    assert!(rendered.contains("src/main.go:7::Run  ·  calls · indexed"), "{rendered}");
    assert!(!rendered.contains("site not captured"), "{rendered}");
    assert_eq!(rendered.matches("Run").count(), 1, "the site is named once: {rendered}");
    // The typed row keeps the exact operand and the evidence tags.
    let typed = crate::ProductDto::new(&view);
    assert_eq!(typed.records[0].operand.as_deref(), Some("/p::src/main.go:7::Run"));
    assert!(typed.records[0].tags.iter().any(|tag| tag == "site not captured"));
}

fn outline_of(count: usize) -> OutlineTree {
    let entries = (0..count)
        .map(|index| {
            let node = OutlineNode {
                symbol: symbol_key(&format!("{PROJECT}::src/m.go:{}::Item{index:03}", index + 1)),
                children: Box::new([]),
            };
            let label = format!("{PROJECT}::src/m.go:{}::Item{index:03}", index + 1);
            let mut resolve = |_symbol| {
                Some((
                    crate::Identity::parse(&label),
                    Some(DeclarationKind::Function),
                ))
            };
            OutlineEntry::resolve(&node, &mut resolve)
        })
        .collect::<Vec<_>>();
    OutlineTree::new(crate::Identity::parse(PROJECT), entries, OutlineExtent::Complete)
}

#[test]
fn an_outline_pages_by_cursor_in_outline_order() {
    let tree = outline_of(400);
    let mut names = Vec::new();
    let mut request = WindowRequest::first();
    loop {
        let window = tree.clone().windowed(request).expect("window");
        let answer = Answer::Outline(Box::new(window.clone()));
        let cursor = match answer.cursor() {
            Some(ContinuationCursor::Window(cursor)) => Some(cursor),
            _ => None,
        };
        let token = cursor.map(WindowCursor::encode);
        let rendered = text::answer_with_detail(
            &answer,
            crate::Theme::plain(),
            Detail::Summary,
            token.as_deref(),
            ContinuationTarget::CliOption,
        );
        names.extend(window.roots().iter().map(OutlineEntry::name));
        match cursor {
            Some(cursor) => {
                assert!(rendered.contains("more declarations;"), "{rendered}");
                assert!(rendered.contains(&cursor.encode()), "{rendered}");
                request = WindowRequest::first().with_cursor(cursor);
            }
            None => break,
        }
    }
    assert_eq!(names.len(), 400);
    assert_eq!(names[0], "Item000");
    assert_eq!(names[399], "Item399");
    let typed = crate::OutlineDto::new(&tree.windowed(WindowRequest::first().with_limit(10)).expect("window"));
    assert_eq!(typed.declarations, 400, "the typed count is the whole outline's");
    assert_eq!(typed.roots.len(), 10);
    assert!(typed.window.is_some_and(|window| window.more));
}

#[test]
fn window_kinds_never_resume_each_others_cursors() {
    let cursor = WindowCursor::new(WindowKind::Rows, 3, 1);
    let tree = outline_of(10);
    let fault = tree
        .windowed(WindowRequest::first().with_cursor(cursor))
        .expect_err("a row cursor does not page an outline");
    assert_eq!(fault.slug(), FaultSlug::CursorMismatch);
}

#[test]
fn list_rows_drop_two_identity_fields_below_full_and_keep_every_field_at_full() {
    let page = window_members(type_with_members(6), WindowRequest::first()).expect("window");
    let standard = PageDto::from_projection(&PageProjection::new(&page, Detail::Standard));
    let row = &standard.members[0].members[0];
    assert!(row.identity.project.is_none() && row.identity.semantic_data.is_none());
    assert!(!row.identity.coordinate.is_empty() && row.identity.line.is_some());
    assert!(row.signature.is_some(), "standard rows carry their signature");
    let summary = PageDto::from_projection(&PageProjection::new(&page, Detail::Summary));
    assert!(summary.members[0].members[0].signature.is_none(), "a summary names");
    let full = PageDto::from_projection(&PageProjection::new(&page, Detail::Full));
    let row = &full.members[0].members[0];
    assert!(row.identity.project.is_some() && row.identity.semantic_data.is_some());
    // The page's own identity is never lean.
    assert!(standard.identity.project.is_some() && standard.identity.semantic_data.is_some());
}

#[test]
fn a_neighbourhood_pages_by_cursor_through_the_summary_text_and_reports_its_window() {
    let records = (0..120)
        .map(|index| {
            let label = format!(
                "{PROJECT}::src/n_{index:03}.go:{}::Neighbour{index:03}",
                index + 1
            );
            let row = Row::in_package(
                RowId::Symbol(symbol_key(&label)),
                basis(),
                package_key(PROJECT),
                &label,
            )
            .with_kind(DeclarationKind::Function);
            crate::Record::from_row(&row)
        })
        .collect::<Vec<_>>();
    let list = crate::RecordList::new(
        DECLARATION,
        crate::CoverageLine::new(&[backend_library::Coverage::Complete], Some(120)),
        records,
    );
    let mut arguments = serde_json::Map::new();
    arguments.insert("coordinate".to_owned(), DECLARATION.into());
    let mut request = WindowRequest::first();
    let (mut seen, mut pages) = (0, 0);
    loop {
        let window = list.clone().windowed(request).expect("window");
        let typed = crate::RecordListDto::new(&window);
        let answer = Answer::Records(Box::new(window));
        let cursor = match answer.cursor() {
            Some(ContinuationCursor::Window(cursor)) => Some(cursor),
            _ => None,
        };
        let token = cursor.map(WindowCursor::encode);
        let rendered = markdown::summary_with_cursor(
            &answer,
            token.as_deref(),
            ContinuationTarget::McpCall {
                tool: "backend.related",
                arguments: &arguments,
            },
        );
        assert!(!rendered.contains("~lanes"), "{rendered}");
        seen += rendered.lines().filter(|line| line.starts_with("• `Neighbour")).count();
        pages += 1;
        match cursor {
            Some(cursor) => {
                assert!(
                    rendered.contains("more rows; next page: backend.related"),
                    "{rendered}"
                );
                assert!(typed.more && typed.window.as_ref().is_some_and(|window| window.more));
                request = WindowRequest::first().with_cursor(cursor);
            }
            None => {
                assert!(!typed.more);
                break;
            }
        }
        assert!(pages < 10);
    }
    assert_eq!(seen, 120, "every neighbour is reachable");
    assert!(pages >= 2);
}
