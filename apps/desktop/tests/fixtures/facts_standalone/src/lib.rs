//! Facts a reader weighs before using a declaration, one of each.

/// Makes a widget from a name.
///
/// The name is trimmed first.
///
/// # Errors
/// Fails when the name is empty.
///
/// # Panics
/// Never panics.
#[deprecated(since = "1.2.0", note = "use `fresh` instead")]
pub fn stale(name: &str) -> Result<u8, String> {
    if name.trim().is_empty() {
        Err("empty".to_owned())
    } else {
        Ok(1)
    }
}

/// Makes a widget the current way.
pub fn fresh() -> u8 {
    1
}

/// A contract with one method to write and one that comes free.
pub trait Service {
    /// Runs the service once.
    ///
    /// A second paragraph: the ledger shows it only when a member keeps
    /// its whole documentation.
    fn execute(&self) -> u8;

    /// Describes the service.
    fn describe(&self) -> String {
        String::new()
    }
}

/// A plain record.
pub struct Plain {
    /// The old label.
    #[deprecated = "read `name` instead"]
    pub label: u8,
    /// The name.
    pub name: String,
}
