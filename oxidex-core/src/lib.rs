//! Oxidex's search engine: walk folders into an `Index`, then search
//! the names in it. No UI; the frontends build on this.
pub mod history;
pub mod index;
mod matching;
pub mod score;
pub mod search;
pub mod store;
pub mod walk;

pub use history::{History, Record};
pub use index::Index;
pub use score::{Scorer, default_scorers};
pub use search::{Cancel, Cancelled, Hit, Sources, Tier, search};
pub use walk::{Walk, walk};
