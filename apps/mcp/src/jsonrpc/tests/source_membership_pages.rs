//! Actual transport encoding of complete pages; no owner authority is fabricated.
use super::*;
use backend_library::{
    PackageSourceMembershipCursorV1, PackageSourceMembershipExclusionsV1,
    PackageSourceMembershipFileV1, PackageSourceMembershipLanguageV1,
    PackageSourceMembershipPageRequestV1, PackageSourceMembershipPageResultV1,
    PackageSourceMembershipScopeV1,
};

fn rows() -> Vec<PackageSourceMembershipFileV1> {
    (0_u32..72)
        .map(|n| {
            let mut key = [0; 32];
            key[..4].copy_from_slice(&n.to_be_bytes());
            // The initial page can fit, while the next page contains dense paths.
            let path = if n < 16 {
                format!("src/{n}.py")
            } else {
                format!("src/{n}/{}.py", "long_路径".repeat(300))
            };
            PackageSourceMembershipFileV1 {
                file_key: key,
                path,
                language: PackageSourceMembershipLanguageV1::Python,
                content_version: [0xa5; 32],
                source_identity: Some([0x5a; 32]),
            }
        })
        .collect()
}

pub(super) fn page_for(
    request: &PackageSourceMembershipPageRequestV1,
    rows: &[PackageSourceMembershipFileV1],
) -> PackageSourceMembershipPageResultV1 {
    let project_key = package_key(request.package.as_str()).to_bytes();
    let start = request
        .cursor
        .as_ref()
        .map_or(0, |cursor| cursor.ordinal as usize + 1);
    let end = (start + usize::from(request.limit)).min(rows.len());
    let next = (end < rows.len()).then(|| PackageSourceMembershipCursorV1 {
        schema: 1,
        package: request.package.clone(),
        project_key,
        source_relation_root: [11; 32],
        source_version: [12; 32],
        membership_page: 0,
        membership_offset: (end - 1) as u16,
        ordinal: (end - 1) as u32,
        last_file_key: rows[end - 1].file_key,
    });
    PackageSourceMembershipPageResultV1::Page {
        package: request.package.clone(),
        project_key,
        source_relation_root: [11; 32],
        source_version: [12; 32],
        file_count: rows.len() as u32,
        start_offset: start as u32,
        scope: PackageSourceMembershipScopeV1::IndexedProjectMembership,
        exclusions: PackageSourceMembershipExclusionsV1::NotCaptured,
        files: rows[start..end].to_vec().into_boxed_slice(),
        next,
    }
}

fn invoke(
    server: &mut Server<Fake>,
    selected: &PackageSourceMembershipPageRequestV1,
    raw: bool,
    compact: bool,
) -> Value {
    let call = if raw {
        json!({"name":"backend.surface","arguments":{"command":SurfaceCommand::PackageSourceMembership { request:selected.clone() },"detail":"full"}})
    } else {
        json!({"name":"backend.package_source_membership","arguments":{"request":selected,"detail":"full"}})
    };
    let call = if compact {
        McpExposure::RegisteredCall.expose_call(call)
    } else {
        call
    };
    request(server, "tools/call", &call)
}

#[test]
fn first_and_later_dense_pages_fit_with_exact_complete_iteration_on_both_routes() {
    let original = rows();
    for (raw, compact) in [(false, false), (false, true), (true, false)] {
        for initial_limit in [16, 128] {
            let mut server = ready(Fake {
                membership_rows: Some(original.clone()),
                ..Fake::default()
            });
            let mut selected = PackageSourceMembershipPageRequestV1::first(
                PackageReference::parse(PROJECT).expect("local"),
            );
            selected.limit = initial_limit;
            let mut union = Vec::new();
            loop {
                let caller_cursor = selected.cursor.clone();
                let before = server.product.surface_commands.len();
                let response = invoke(&mut server, &selected, raw, compact);
                assert_context_bounded(&response);
                assert_eq!(response["result"]["isError"], false, "{response}");
                let encoded = if raw {
                    &response["result"]["structuredContent"]["surface"]["data"]
                } else {
                    &response["result"]["structuredContent"]["package_source_membership_page"]
                };
                let page: PackageSourceMembershipPageResultV1 =
                    serde_json::from_value(encoded.clone()).expect("complete native page");
                assert!(page.admits_request(&selected));
                let PackageSourceMembershipPageResultV1::Page {
                    source_relation_root,
                    source_version,
                    file_count,
                    start_offset,
                    files,
                    next,
                    ..
                } = page
                else {
                    panic!("page")
                };
                assert_eq!(file_count, original.len() as u32);
                assert_eq!(start_offset, union.len() as u32);
                union.extend(files.into_vec());
                for command in &server.product.surface_commands[before..] {
                    let SurfaceCommand::PackageSourceMembership { request } = command else {
                        panic!("membership only")
                    };
                    assert_eq!(
                        request.cursor, caller_cursor,
                        "oversized unreturned candidate cannot advance frontier"
                    );
                    assert!(request.limit <= selected.limit);
                }
                if let Some(cursor) = next {
                    selected = selected
                        .with_selection(source_relation_root, source_version)
                        .with_cursor(cursor);
                } else {
                    break;
                }
            }
            assert_eq!(
                union, original,
                "all complete paths and identities survive in exact key order"
            );
            let attempted: Vec<_> = server
                .product
                .surface_commands
                .iter()
                .map(|command| {
                    let SurfaceCommand::PackageSourceMembership { request } = command else {
                        panic!("membership")
                    };
                    request.limit
                })
                .collect();
            assert!(
                attempted.windows(2).any(|pair| pair[1] < pair[0]),
                "actual encoded payload must force adaptation"
            );
        }
    }
}

#[test]
fn invalid_arguments_never_reach_owner_and_next_valid_call_works() {
    for raw in [false, true] {
        let mut server = ready(Fake {
            membership_rows: Some(rows()),
            ..Fake::default()
        });
        let mut selected = PackageSourceMembershipPageRequestV1::first(
            PackageReference::parse(PROJECT).expect("local"),
        );
        for limit in [0, 129] {
            selected.limit = limit;
            let response = invoke(&mut server, &selected, raw, false);
            assert!(
                response.get("error").is_some() || response["result"]["isError"] == true,
                "{response}"
            );
            assert!(server.product.surface_commands.is_empty());
        }
        selected.limit = 1;
        assert_eq!(
            invoke(&mut server, &selected, raw, false)["result"]["isError"],
            false
        );
    }
}

#[test]
fn retries_keep_pinned_extent_and_stop_on_native_stale_or_wrong_selection() {
    let selected = PackageSourceMembershipPageRequestV1 {
        limit: 128,
        ..PackageSourceMembershipPageRequestV1::first(
            PackageReference::parse(PROJECT).expect("local"),
        )
    };
    for raw in [false, true] {
        let large = page_for(&selected, &rows());
        let stale = PackageSourceMembershipPageResultV1::Stale {
            package: selected.package.clone(),
            current_source_relation_root: Some([99; 32]),
            current_source_version: Some([98; 32]),
        };
        let mut server = ready(Fake {
            surface_replies: [
                SurfaceReply::PackageSourceMembershipPage(large.clone()),
                SurfaceReply::PackageSourceMembershipPage(stale.clone()),
            ]
            .into(),
            ..Fake::default()
        });
        let response = invoke(&mut server, &selected, raw, false);
        assert_eq!(response["result"]["isError"], false, "{response}");
        let returned = if raw {
            &response["result"]["structuredContent"]["surface"]["data"]
        } else {
            &response["result"]["structuredContent"]["package_source_membership_page"]
        };
        assert_eq!(returned, &json!(stale));
        assert_eq!(server.product.surface_commands.len(), 2);
        let SurfaceCommand::PackageSourceMembership { request: retry } =
            &server.product.surface_commands[1]
        else {
            panic!("membership")
        };
        assert_eq!(retry.expected_source_relation_root, Some([11; 32]));
        assert_eq!(retry.expected_source_version, Some([12; 32]));
        assert_eq!(retry.cursor, selected.cursor);
        let mut smaller = selected.clone();
        smaller.limit = 64;
        for mutation in 0..4 {
            let mut wrong = page_for(&smaller, &rows());
            let PackageSourceMembershipPageResultV1::Page {
                package,
                project_key,
                source_relation_root,
                file_count,
                ..
            } = &mut wrong
            else {
                panic!("page")
            };
            match mutation {
                0 => *package = PackageReference::parse("/other").expect("local"),
                1 => *project_key = [99; 32],
                2 => *source_relation_root = [99; 32],
                _ => *file_count += 1,
            }
            let mut server = ready(Fake {
                surface_replies: [
                    SurfaceReply::PackageSourceMembershipPage(large.clone()),
                    SurfaceReply::PackageSourceMembershipPage(wrong),
                ]
                .into(),
                ..Fake::default()
            });
            let response = invoke(&mut server, &selected, raw, false);
            assert_eq!(
                response["result"]["isError"], true,
                "mutation={mutation} {response}"
            );
            assert_eq!(
                server.product.surface_commands.len(),
                2,
                "invalid reply cannot trigger another attempt"
            );
        }
    }
}

#[test]
fn one_file_oversize_is_an_honest_fault_not_hidden_truncation() {
    // A legal long package is repeated in the exact native cursor and the raw
    // command continuation. This can exceed the envelope even with one row.
    // ProductText permits non-NUL control bytes. JSON must retain their full
    // escaped spelling; two copies alone consume the 48 KiB byte budget.
    let package = PackageReference::parse("\u{1}".repeat(4096)).expect("bounded escaped package");
    let mut selected = PackageSourceMembershipPageRequestV1::first(package);
    selected.limit = 1;
    let mut files = rows();
    files[0].path = "\"".repeat(4096);
    let mut server = ready(Fake {
        membership_rows: Some(files),
        ..Fake::default()
    });
    let response = invoke(&mut server, &selected, true, false);
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert_eq!(
        response["result"]["structuredContent"]["cause"],
        "oversized"
    );
    assert_eq!(server.product.surface_commands.len(), 1);
}
