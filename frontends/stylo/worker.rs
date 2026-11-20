//! Supervised native Stylo process entry point.
fn main() {
    if backend_frontend_stylo::worker_protocol::serve(
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    )
    .is_err()
    {
        std::process::exit(1);
    }
}
