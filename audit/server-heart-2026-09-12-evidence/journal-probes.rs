#[test]
fn audit_complete_fact_before_head_cannot_recover() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new("audit-before-head")?;
    let paths = fixture.paths();
    write_fact(&paths, verified_input(91, 92))?;
    let limits = PublicationLimits::new(nonzero(1)?, nonzero(1)?)?;
    assert!(matches!(DurablePublisher::reopen(&paths, limits), Err(PublicationOpenError::MissingHead)));
    Ok(())
}

#[test]
fn audit_head_temp_blocks_reopen_of_valid_previous_publication() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new("audit-head-temp")?;
    let paths = fixture.paths();
    write_fact_and_head(&paths, verified_input(93, 94))?;
    fs::write(paths.head_temp(), b"interrupted successor head write")?;
    let limits = PublicationLimits::new(nonzero(1)?, nonzero(1)?)?;
    assert!(matches!(DurablePublisher::reopen(&paths, limits), Err(PublicationOpenError::HeadTempPresent)));
    Ok(())
}

