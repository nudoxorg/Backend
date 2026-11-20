//! Bounded BLAKE3 helper for the synthetic NuGet source-proof control.
use std::io::{self, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = Vec::new();
    io::stdin().lock().take(4097).read_to_end(&mut input)?;
    if input.len() > 4096 {
        return Err("proof input exceeds 4096 bytes".into());
    }
    println!("{}", blake3::hash(&input).to_hex());
    Ok(())
}
