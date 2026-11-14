//! One-shot native SQLite parser worker; never opens a database.
use backend_sql_contract::{MAX_CONTEXT_BYTES, MAX_SOURCE_BYTES, Request};
use std::io::{Read, Write};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(((MAX_SOURCE_BYTES + MAX_CONTEXT_BYTES) * 4 + 4096) as u64)
        .read_to_end(&mut input)?;
    let request: Request = serde_json::from_slice(&input)?;
    serde_json::to_writer(
        std::io::stdout().lock(),
        &backend_frontend_sqlite_experimental::analyze_native(&request),
    )?;
    std::io::stdout().flush()?;
    Ok(())
}
