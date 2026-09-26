//! Reads a project's settings from TOML text. The journeys' "your project":
//! it pins `toml = "0.8.23"` and calls `toml::from_str`.

use toml::Value;

/// The settings a project keeps in `nudox.toml`.
pub struct Settings {
    /// Everything the file says, parsed.
    pub table: Value,
}

/// Parses `text` as TOML settings.
///
/// # Errors
/// The text is not valid TOML.
pub fn read_settings(text: &str) -> Result<Settings, toml::de::Error> {
    let table: Value = toml::from_str(text)?;
    Ok(Settings { table })
}

/// The `name` key, when the settings have one.
pub fn project_name(settings: &Settings) -> Option<&str> {
    settings.table.get("name").and_then(Value::as_str)
}
