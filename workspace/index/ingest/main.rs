//! The Remote-deployment ingestor entry point.

fn main() {
    if let Err(error) = run() {
        eprintln!("ingest: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    use index::{
        engine::OpenCatalog, ingest::HomebrewIngestor, migrations::migrate_to_v4,
        store::writer::CatalogWriter,
    };
    use std::time::Duration;

    let catalog_path = std::env::var_os("NUDOX_CATALOG_PATH").map_or_else(
        || std::path::PathBuf::from("catalog.dolt"),
        std::path::PathBuf::from,
    );
    let watermark_dir = std::env::var_os("NUDOX_WATERMARK_DIR").map_or_else(
        || std::path::PathBuf::from("watermarks"),
        std::path::PathBuf::from,
    );
    let formula_url = std::env::var("NUDOX_HOMEBREW_URL")
        .unwrap_or_else(|_| index::ingest::homebrew::DEFAULT_FORMULA_URL.to_owned());
    let engine = <index::engine::Configured as OpenCatalog>::open_at_path(&catalog_path)?;
    migrate_to_v4(&engine)?;
    let writer = CatalogWriter::new(engine);
    // The fork's versioned store is in memory. This process keeps one catalog
    // for the life of the loop; a restart opens a fresh ledger.
    let facts = std::sync::Mutex::new(index::engine::turso_vc::VersionedCatalog::open()?);
    let mut ingestor =
        HomebrewIngestor::new(&writer, watermark_dir, formula_url)?.with_facts(&facts);
    let cadence = match ingestor.cadence() {
        index::ingest::PollCadence::EverySeconds(seconds) => Duration::from_secs(seconds),
    };

    loop {
        let now = chrono::Utc::now().timestamp_millis();
        let outcome = ingestor.drive_once(now)?;
        eprintln!("ingest: homebrew {outcome:?}");
        std::thread::sleep(cadence);
    }
}
