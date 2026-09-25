//! The desktop capture harness: the gallery command line (`capture`, `film`,
//! `motion-report`, `storm`, `matrix`, `lint`, `perf`, `verify`, `list`)
//! over the real desktop shell on the fixture index
//! (`backend_desktop::harness`). `backend-desktop-gui-harness list` names
//! the scenes; scripts may use `route symbol present::glyph::RelationLabel`.

#[cfg(feature = "visual-harness")]
fn main() -> std::process::ExitCode {
    facet::gallery::cli::main(facet::gallery::cli::Registry {
        name: "backend-desktop-gui-harness",
        all: backend_desktop::harness::scenes,
    })
}

#[cfg(not(feature = "visual-harness"))]
fn main() -> std::process::ExitCode {
    eprintln!("backend-desktop-gui-harness requires the visual-harness feature");
    std::process::ExitCode::FAILURE
}
