//! `facet-gallery`: FACET's scenes through the gallery command line
//! (`facet::gallery::cli`; `facet-gallery help` lists the commands).

use facet::gallery::{self, cli};

fn main() -> std::process::ExitCode {
    cli::main(cli::Registry {
        name: "facet-gallery",
        all: gallery::all,
    })
}
