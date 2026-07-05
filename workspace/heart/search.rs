use serde::{Deserialize, Serialize};

use crate::score::Scored;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Our page which handles and groups results
pub struct Page<T> {
    /// The scored hits on this page, in descending relevance.
    pub items: Vec<Scored<T>>,

    /// The opaque [`crate::Cursor`] token to resume after the last item, or `None`
    /// if this is the last page.
    pub next: Option<String>,
}
