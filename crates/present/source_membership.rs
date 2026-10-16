//! Complete native membership pages selected under each surface's real budget.
use crate::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};
use backend_library::{
    PackageSourceMembershipPageRequestV1, PackageSourceMembershipPageResultV1, SurfaceCommand,
    SurfaceReply,
};

/// Retains the caller's exact cursor while selecting a smaller complete page.
/// Unlike semantic-version/graph cursors, membership cursors bind a member
/// position, not the row limit. Both initial and resumed reads may shrink.
pub struct PackageSourceMembershipSelection {
    request: PackageSourceMembershipPageRequestV1,
    basis: Option<([u8; 32], [u8; 32], [u8; 32], u32)>,
    page: bool,
}

impl PackageSourceMembershipSelection {
    /// Validates all original operands before any owner call.
    pub fn new(request: &PackageSourceMembershipPageRequestV1) -> Result<Self, Fault> {
        if !request.has_admissible_shape() {
            return Err(Fault::usage(
                "request",
                "source membership request is malformed",
            ));
        }
        Ok(Self {
            request: request.clone(),
            basis: None,
            page: false,
        })
    }

    /// Exact request for the next attempt; a retry never advances its cursor.
    pub fn request(&self) -> &PackageSourceMembershipPageRequestV1 {
        &self.request
    }

    /// Re-admits every candidate before rendering and pins its complete extent.
    pub fn admit_reply(&mut self, reply: &SurfaceReply) -> Result<(), Fault> {
        backend_library::admit_surface_reply(
            &SurfaceCommand::PackageSourceMembership {
                request: self.request.clone(),
            },
            reply,
        )
        .map_err(|error| {
            Fault::from_client_error(
                &backend_client::ClientError::Protocol(error.to_string()),
                Operand::Whole,
            )
        })?;
        self.page = false;
        if let SurfaceReply::PackageSourceMembershipPage(
            PackageSourceMembershipPageResultV1::Page {
                project_key,
                source_relation_root,
                source_version,
                file_count,
                ..
            },
        ) = reply
        {
            let basis = (
                *project_key,
                *source_relation_root,
                *source_version,
                *file_count,
            );
            if self.basis.is_some_and(|original| original != basis) {
                return Err(Fault::new(
                    FaultSlug::Freshness,
                    Operand::Whole,
                    Cause::new(
                        CauseSlug::Moved,
                        "source membership changed while selecting a bounded page; restart the read explicitly",
                    ),
                    Affordance::None,
                ));
            }
            self.basis = Some(basis);
            self.request.expected_source_relation_root = Some(*source_relation_root);
            self.request.expected_source_version = Some(*source_version);
            self.page = true;
        }
        Ok(())
    }

    /// Only an admitted page refused for size can trigger another owner read.
    /// A one-file page or typed stale/unavailable result is returned atomically.
    pub fn shrink_after(&mut self, fault: &Fault) -> bool {
        if !self.page || self.request.limit <= 1 || fault.cause().slug() != CauseSlug::Oversized {
            return false;
        }
        self.request.limit = self.request.limit.div_ceil(2);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::{
        PackageReference, PackageSourceMembershipCursorV1, PackageSourceMembershipExclusionsV1,
        PackageSourceMembershipFileV1, PackageSourceMembershipLanguageV1,
        PackageSourceMembershipScopeV1,
    };

    fn request() -> PackageSourceMembershipPageRequestV1 {
        PackageSourceMembershipPageRequestV1::first(
            PackageReference::parse("/membership").expect("local"),
        )
    }
    fn page(request: &PackageSourceMembershipPageRequestV1) -> SurfaceReply {
        let start = request
            .cursor
            .as_ref()
            .map_or(0, |cursor| cursor.ordinal + 1);
        let end = (start + u32::from(request.limit)).min(72);
        let files: Vec<_> = (start..end)
            .map(|n| {
                let mut file_key = [0; 32];
                file_key[..4].copy_from_slice(&n.to_be_bytes());
                PackageSourceMembershipFileV1 {
                    file_key,
                    path: format!("src/{n}.py"),
                    language: PackageSourceMembershipLanguageV1::Python,
                    content_version: [4; 32],
                    source_identity: Some([5; 32]),
                }
            })
            .collect();
        let project_key = backend_library::package_key(request.package.as_str()).to_bytes();
        let next = (end < 72).then(|| PackageSourceMembershipCursorV1 {
            schema: 1,
            package: request.package.clone(),
            project_key,
            source_relation_root: [2; 32],
            source_version: [3; 32],
            membership_page: 0,
            membership_offset: (end - 1) as u16,
            ordinal: end - 1,
            last_file_key: files.last().expect("page").file_key,
        });
        SurfaceReply::PackageSourceMembershipPage(PackageSourceMembershipPageResultV1::Page {
            package: request.package.clone(),
            project_key,
            source_relation_root: [2; 32],
            source_version: [3; 32],
            file_count: 72,
            start_offset: start,
            scope: PackageSourceMembershipScopeV1::IndexedProjectMembership,
            exclusions: PackageSourceMembershipExclusionsV1::NotCaptured,
            files: files.into_boxed_slice(),
            next,
        })
    }
    fn oversized() -> Fault {
        crate::oversized_fault(crate::BudgetExceeded {
            bytes: 50000,
            budget: 49152,
        })
    }

    #[test]
    fn retries_pin_selection_and_retain_the_original_cursor_without_page_size_credit() {
        let first = page(&request());
        let SurfaceReply::PackageSourceMembershipPage(PackageSourceMembershipPageResultV1::Page {
            next: Some(cursor),
            ..
        }) = first
        else {
            panic!("cursor")
        };
        let resumed = request()
            .with_selection([2; 32], [3; 32])
            .with_cursor(cursor.clone());
        let mut selection = PackageSourceMembershipSelection::new(&resumed).expect("valid resume");
        for expected in [32, 16, 8, 4, 2, 1] {
            assert_eq!(selection.request().limit, expected);
            assert_eq!(selection.request().cursor.as_ref(), Some(&cursor));
            selection
                .admit_reply(&page(selection.request()))
                .expect("exact native page at every limit");
            assert_eq!(selection.shrink_after(&oversized()), expected > 1);
        }
    }

    #[test]
    fn exact_request_admission_refuses_wrong_package_root_version_project_extent_and_key() {
        let request = request().with_selection([2; 32], [3; 32]);
        for mutation in 0..7 {
            let mut reply = page(&request);
            let SurfaceReply::PackageSourceMembershipPage(
                PackageSourceMembershipPageResultV1::Page {
                    package,
                    project_key,
                    source_relation_root,
                    source_version,
                    start_offset,
                    files,
                    next,
                    ..
                },
            ) = &mut reply
            else {
                panic!("page")
            };
            match mutation {
                0 => *package = PackageReference::parse("/other").expect("local"),
                1 => *project_key = [9; 32],
                2 => *source_relation_root = [9; 32],
                3 => *source_version = [9; 32],
                4 => *start_offset = 1,
                5 => {
                    files[1].file_key = files[0].file_key;
                }
                _ => next.as_mut().expect("next").last_file_key = [9; 32],
            }
            assert!(
                backend_library::admit_surface_reply(
                    &SurfaceCommand::PackageSourceMembership {
                        request: request.clone()
                    },
                    &reply
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn typed_stale_and_unavailable_stop_and_changed_total_fails_closed() {
        let mut selection = PackageSourceMembershipSelection::new(&request()).expect("request");
        selection
            .admit_reply(&page(selection.request()))
            .expect("page");
        assert!(selection.shrink_after(&oversized()));
        let mut moved = page(selection.request());
        let SurfaceReply::PackageSourceMembershipPage(PackageSourceMembershipPageResultV1::Page {
            file_count,
            ..
        }) = &mut moved
        else {
            panic!("page")
        };
        *file_count += 1;
        assert_eq!(
            selection
                .admit_reply(&moved)
                .expect_err("extent moved")
                .slug(),
            FaultSlug::Freshness
        );
        for result in [
            PackageSourceMembershipPageResultV1::Stale {
                package: request().package,
                current_source_relation_root: Some([8; 32]),
                current_source_version: Some([9; 32]),
            },
            PackageSourceMembershipPageResultV1::Unavailable {
                package: request().package,
                reason:
                    backend_library::PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
            },
        ] {
            selection
                .admit_reply(&SurfaceReply::PackageSourceMembershipPage(result))
                .expect("typed terminal");
            assert!(!selection.shrink_after(&oversized()));
        }
    }

    #[test]
    fn invalid_request_and_nonbudget_fault_never_authorize_retry() {
        let mut selected = request();
        selected.limit = 0;
        assert!(PackageSourceMembershipSelection::new(&selected).is_err());
        selected.limit = 129;
        assert!(PackageSourceMembershipSelection::new(&selected).is_err());
        let mut selection = PackageSourceMembershipSelection::new(&request()).expect("request");
        assert!(!selection.shrink_after(&oversized()), "no native page yet");
        selection
            .admit_reply(&page(selection.request()))
            .expect("page");
        assert!(!selection.shrink_after(&Fault::usage("request", "invalid")));
    }
}
