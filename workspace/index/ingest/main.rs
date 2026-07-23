//! The Remote-deployment ingestor entry point (INDEX-PLAN ID-7, §11 loop 8).
//!
//! This binary is intentionally **thin**: all behaviour lives in the `ingestor`
//! library. In a real Remote deployment it would open the catalog writer (real
//! DoltLite engine), wire a live `reqwest`-backed [`FeedTransport`] and the
//! catalog's watermark tables behind [`WatermarkStore`], construct the
//! followers + [`GitMonitor`], and drive them on their cadences via
//! [`FollowerDriver`].
//!
//! Those wirings depend on the deployment bootstrap (engine open path, config,
//! async runtime, and the not-yet-existing watermark read/write API on the
//! index crate — see the crate report), which is out of this workstream's
//! scope. Until that lands, `main` documents the intended composition and exits
//! cleanly so the crate builds and links as a binary target.

fn main() {
    // Deployment bootstrap wires:
    //   let engine = index::engine::dolt::DoltEngine::open(&catalog_path)?;
    //   let writer = index::store::writer::CatalogWriter::new(engine);
    //   let watermarks = /* catalog-backed WatermarkStore adapter */;
    //   let driver = ingestor::FollowerDriver::new(&writer, &watermarks);
    //   let brew = ingestor::homebrew::HomebrewFollower::new(reqwest_transport);
    //   loop { driver.drive_once(&brew, now_ms())?; sleep(cadence); }
    eprintln!(
        "ingestor: library-first; the Remote process bootstrap (engine + \
         transport + watermark adapter) is wired by the deployment layer. \
         See lib.rs and the crate report."
    );
}
