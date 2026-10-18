//! RDM core: segmented download engine with resume.

pub mod engine;
pub mod events;
pub mod state;

pub use engine::{download, job_from_url, Job, Limits};
pub use events::Progress;
