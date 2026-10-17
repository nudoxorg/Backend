use super::product_view;
use backend_library::{
    PackageCoordinate, ProductText, RegistryPackageDiscoveryObservation, SurfaceReply,
};

#[test]
fn exact_metadata_failure_cause_survives_cli_text_and_mcp_structured_presentation()
-> Result<(), Box<dyn std::error::Error>> {
    let package = PackageCoordinate::parse("pkg:pypi/requests@2.34.2")
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    for reason in [
        "metadata request failed: Io(HTTP 503)",
        "metadata request failed: Decode",
        "a newer point observation was committed before this response; retry",
        "registry metadata authority changed during the request; retry",
        "registry metadata is offline and no current admitted cached observation is available",
    ] {
        let observation = RegistryPackageDiscoveryObservation::Unavailable {
            source: Some([7; 32]),
            reason: ProductText::from_static(reason),
        };
        let view = product_view(&SurfaceReply::PackageDiscovery {
            package: package.clone(),
            observation: observation.clone(),
        });
        assert!(crate::text::product(&view, crate::Theme::plain()).contains(reason));
        assert!(crate::markdown::product(&view).contains(reason));
        let dto = crate::dto::ProductDto::new(&view);
        let value = serde_json::to_value(&dto)?;
        assert_eq!(
            value["package_discovery"]["observation"]["state"],
            "unavailable"
        );
        assert_eq!(value["package_discovery"]["observation"]["reason"], reason);
        let decoded: crate::dto::ProductDto = serde_json::from_value(value)?;
        let projection = decoded
            .package_discovery
            .ok_or_else(|| std::io::Error::other("metadata cause projection disappeared"))?;
        assert_eq!(projection.observation, observation);
        assert_eq!(projection.package, package);
        assert!(
            view.records().is_empty(),
            "failure is not an acquired package"
        );
    }
    Ok(())
}
