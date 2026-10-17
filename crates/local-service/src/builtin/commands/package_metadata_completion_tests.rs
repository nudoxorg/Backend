use super::{BuiltinModelError, CommandReply, package_metadata_completion_reply};
use backend_library::{
    CommandFailure, PackageCoordinate, PackageReference, ProductText,
    RegistryPackageDiscoveryObservation as Observation, SurfaceCommand, SurfaceReply,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn package() -> Result<PackageCoordinate, std::io::Error> {
    PackageCoordinate::parse("pkg:pypi/requests@2.34.2")
        .map_err(|error| std::io::Error::other(format!("{error:?}")))
}

fn unavailable(reason: &'static str) -> Observation {
    Observation::Unavailable {
        source: Some([7; 32]),
        reason: ProductText::from_static(reason),
    }
}

fn generic(package: &PackageCoordinate) -> CommandReply {
    CommandReply::Surface(SurfaceReply::PackageDiscovery {
        package: package.clone(),
        observation: unavailable("registry package metadata has not been observed yet"),
    })
}

/// Both CLI and MCP use the same strict daemon reply protocol. Preserve the
/// whole typed cause through that boundary, not merely a matching error string.
#[test]
fn exact_metadata_failures_survive_completion_and_shared_client_wire() -> TestResult {
    let package = package()?;
    let request = backend_library::CommandDto::new(
        71,
        backend_library::Command::Surface(SurfaceCommand::Package {
            package: PackageReference::Purl(package.clone()),
        }),
    );
    for reason in [
        "metadata request failed: Io(HTTP 503)",
        "metadata request failed: Decode",
        "a newer point observation was committed before this response; retry",
        "registry metadata authority changed during the request; retry",
        "registry metadata is offline and no current admitted cached observation is available",
    ] {
        let observation = unavailable(reason);
        let reply = package_metadata_completion_reply(
            package.clone(),
            Ok(generic(&package)),
            Some(observation.clone()),
        );
        let expected = CommandReply::Surface(SurfaceReply::PackageDiscovery {
            package: package.clone(),
            observation,
        });
        assert_eq!(reply, expected);
        let dto = backend_library::ReplyDto::new(71, reply);
        let bytes = backend_library::encode_reply_body_bounded(&dto)?;
        let decoded = backend_library::decode_reply_body(&bytes).map_err(std::io::Error::other)?;
        backend_library::admit_reply(&request, &decoded)
            .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
        assert_eq!(decoded.reply, expected);
    }
    let observation = Observation::Missing {
        source: [3; 32],
        proof: [4; 32],
        observed_at_millis: 100,
    };
    assert_eq!(
        package_metadata_completion_reply(
            package.clone(),
            Ok(generic(&package)),
            Some(observation.clone()),
        ),
        CommandReply::Surface(SurfaceReply::PackageDiscovery {
            package,
            observation,
        })
    );
    Ok(())
}

#[test]
fn acquired_catalog_reply_stays_authoritative_over_metadata_failure() -> TestResult {
    let package = package()?;
    let ecosystem = backend_library::RegistryEcosystem::Pypi;
    let native_metadata = backend_library::RegistryNativeMetadata::unavailable(
        ecosystem,
        "source-native metadata was not supplied",
    );
    let record = backend_library::RegistryPackageRecord {
        coordinate: PackageReference::Purl(package.clone()),
        ecosystem,
        name: ProductText::from_static("requests"),
        version: ProductText::from_static("2.34.2"),
        bytes: 4,
        standing: backend_library::RegistryReleaseStanding::Available,
        downloads: backend_library::RegistryDownloadCount::Exact(42),
        facts_version: [9; 32],
        authority: None,
        native_metadata_version: native_metadata
            .identity()
            .map_err(|error| std::io::Error::other(format!("{error:?}")))?,
        native_metadata,
        forge_sources: Box::new([]),
        advisory: backend_library::AdvisoryPackageDto::unknown(),
    };
    let positive = CommandReply::Surface(SurfaceReply::Package(Box::new([record])));
    for observation in [
        unavailable("metadata request failed: Decode"),
        Observation::Missing {
            source: [3; 32],
            proof: [4; 32],
            observed_at_millis: 100,
        },
    ] {
        assert_eq!(
            package_metadata_completion_reply(
                package.clone(),
                Ok(positive.clone()),
                Some(observation)
            ),
            positive
        );
    }
    Ok(())
}

#[test]
fn cancellation_deadline_and_unknown_failures_are_not_replaced_without_an_observation() -> TestResult
{
    let package = package()?;
    for reason in [
        "registry metadata request was cancelled",
        "registry metadata request exceeded its deadline",
    ] {
        let reply = CommandReply::Failed(CommandFailure::InvalidQuery(reason.to_owned()));
        assert_eq!(
            package_metadata_completion_reply(package.clone(), Ok(reply.clone()), None),
            reply
        );
    }
    assert_eq!(
        package_metadata_completion_reply(
            package,
            Err(BuiltinModelError("owner read failed".to_owned())),
            None
        ),
        CommandReply::Failed(CommandFailure::InvalidQuery("owner read failed".to_owned()))
    );
    Ok(())
}
