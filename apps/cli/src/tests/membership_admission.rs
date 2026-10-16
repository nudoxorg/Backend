use super::*;
use backend_library::{
    PackageReference, PackageSourceMembershipCursorV1, PackageSourceMembershipExclusionsV1,
    PackageSourceMembershipFileV1, PackageSourceMembershipLanguageV1,
    PackageSourceMembershipPageRequestV1, PackageSourceMembershipPageResultV1,
    PackageSourceMembershipScopeV1,
};

struct Membership {
    rows: Vec<PackageSourceMembershipFileV1>,
    requests: Vec<PackageSourceMembershipPageRequestV1>,
}
impl Engine for Membership {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        unreachable!("membership never queries revision")
    }
    fn health(&mut self) -> Result<HealthReport, ClientError> {
        unreachable!("membership never queries health")
    }
    fn probe(&mut self, _: Probe<'_>) -> Result<ReplyDto, ClientError> {
        unreachable!("membership never probes")
    }
    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        let SurfaceCommand::PackageSourceMembership { request } = command else {
            panic!("membership only")
        };
        self.requests.push(request.clone());
        let project_key = backend_library::package_key(request.package.as_str()).to_bytes();
        let start = request
            .cursor
            .as_ref()
            .map_or(0, |cursor| cursor.ordinal as usize + 1);
        let end = (start + usize::from(request.limit)).min(self.rows.len());
        let next = (end < self.rows.len()).then(|| PackageSourceMembershipCursorV1 {
            schema: 1,
            package: request.package.clone(),
            project_key,
            source_relation_root: [11; 32],
            source_version: [12; 32],
            membership_page: 0,
            membership_offset: (end - 1) as u16,
            ordinal: (end - 1) as u32,
            last_file_key: self.rows[end - 1].file_key,
        });
        Ok(SurfaceReply::PackageSourceMembershipPage(
            PackageSourceMembershipPageResultV1::Page {
                package: request.package,
                project_key,
                source_relation_root: [11; 32],
                source_version: [12; 32],
                file_count: self.rows.len() as u32,
                start_offset: start as u32,
                scope: PackageSourceMembershipScopeV1::IndexedProjectMembership,
                exclusions: PackageSourceMembershipExclusionsV1::NotCaptured,
                files: self.rows[start..end].to_vec().into_boxed_slice(),
                next,
            },
        ))
    }
}
fn engine() -> Membership {
    Membership {
        rows: (0_u32..72)
            .map(|n| {
                let mut file_key = [0; 32];
                file_key[..4].copy_from_slice(&n.to_be_bytes());
                PackageSourceMembershipFileV1 {
                    file_key,
                    path: if n < 16 {
                        format!("src/{n}.py")
                    } else {
                        format!("src/{n}/{}.py", "long_路径".repeat(300))
                    },
                    language: PackageSourceMembershipLanguageV1::Python,
                    content_version: [0xa5; 32],
                    source_identity: Some([0x5a; 32]),
                }
            })
            .collect(),
        requests: Vec::new(),
    }
}

#[test]
fn cli_actual_formats_fit_complete_dense_continuations_without_skips() {
    for format in ["json", "human", "markdown"] {
        for limit in [16, 128] {
            let options = crate::options::split(&[
                "--format".to_owned(),
                format.to_owned(),
                "--detail".to_owned(),
                "full".to_owned(),
            ])
            .expect("format")
            .0;
            let mut engine = engine();
            let expected = engine.rows.clone();
            let mut selected = PackageSourceMembershipPageRequestV1::first(
                PackageReference::parse("/membership").expect("local"),
            );
            selected.limit = limit;
            let mut union = Vec::new();
            loop {
                let mut selection =
                    backend_present::PackageSourceMembershipSelection::new(&selected)
                        .expect("request");
                let before = engine.requests.len();
                let answer = membership_with_output_admission(
                    &mut engine,
                    &mut selection,
                    || Ok(()),
                    |answer| crate::render::admit_graph_answer(answer, &options),
                )
                .expect("bounded complete page");
                crate::render::try_answer_with_cursor(&answer, &options, None)
                    .expect("actual output fits");
                let Answer::Product(view) = answer else {
                    panic!("product")
                };
                let PackageSourceMembershipPageResultV1::Page {
                    start_offset,
                    file_count,
                    source_relation_root,
                    source_version,
                    files,
                    next,
                    ..
                } = view.package_source_membership_page().expect("typed page")
                else {
                    panic!("page")
                };
                assert_eq!(*file_count, expected.len() as u32);
                assert_eq!(*start_offset, union.len() as u32);
                union.extend(files.iter().cloned());
                for request in &engine.requests[before..] {
                    assert_eq!(request.cursor, selected.cursor);
                }
                if let Some(cursor) = next {
                    selected = selected
                        .with_selection(*source_relation_root, *source_version)
                        .with_cursor(cursor.clone());
                } else {
                    break;
                }
            }
            assert_eq!(union, expected);
            assert!(
                engine
                    .requests
                    .windows(2)
                    .any(|pair| pair[1].limit < pair[0].limit)
            );
        }
    }
}

#[test]
fn expired_scope_prevents_owner_call_and_cancelled_scope_prevents_retry() {
    for expire_after_reply in [false, true] {
        let mut engine = engine();
        let request = PackageSourceMembershipPageRequestV1::first(
            PackageReference::parse("/membership").expect("local"),
        );
        let mut selection =
            backend_present::PackageSourceMembershipSelection::new(&request).expect("request");
        let mut checks = 0;
        let result = membership_with_output_admission(
            &mut engine,
            &mut selection,
            || {
                checks += 1;
                if !expire_after_reply || checks > 1 {
                    Err(Fault::usage("scope", "cancelled"))
                } else {
                    Ok(())
                }
            },
            |_| {
                Err(backend_present::oversized_fault(
                    backend_present::BudgetExceeded {
                        bytes: 50000,
                        budget: 49152,
                    },
                ))
            },
        );
        assert!(result.is_err());
        assert_eq!(engine.requests.len(), usize::from(expire_after_reply));
    }
}

#[test]
fn cli_one_file_oversize_stops_without_advancing_or_clipping() {
    let options = crate::options::Options::plain(crate::options::Format::Json);
    let mut engine = engine();
    let mut request = PackageSourceMembershipPageRequestV1::first(
        PackageReference::parse("\u{1}".repeat(4096)).expect("bounded NUL-free product label"),
    );
    request.limit = 1;
    let mut selection =
        backend_present::PackageSourceMembershipSelection::new(&request).expect("request");
    let fault = membership_with_output_admission(
        &mut engine,
        &mut selection,
        || Ok(()),
        |answer| crate::render::admit_graph_answer(answer, &options),
    )
    .expect_err("complete one-row JSON cannot fit");
    assert_eq!(fault.cause().slug(), backend_present::CauseSlug::Oversized);
    assert_eq!(engine.requests.len(), 1);
    assert_eq!(engine.requests[0], request);
}
