//! Reads `settings.toml` and packs it.

fn main() {
    let text = std::fs::read_to_string("settings.toml").unwrap_or_default();
    let value: toml::Value = toml::from_str(&text).unwrap_or(toml::Value::Boolean(false));
    let packed = bincode::serialize(&value.to_string()).unwrap_or_default();
    println!("{} bytes", packed.len());
}
