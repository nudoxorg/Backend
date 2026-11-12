//! Bounded BLAKE3 helper for the synthetic NuGet source-proof control.
use std::io::{self, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = Vec::new();
    io::stdin().lock().take(1025).read_to_end(&mut input)?;
    if input.len() > 1024 {
        return Err("proof input exceeds 1024 bytes".into());
    }
    println!("{}", blake3::hash(&input).to_hex());
    Ok(())
}
