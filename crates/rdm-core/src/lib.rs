//! RDM core: segmented download engine with resume.

pub mod engine;
pub mod events;
pub mod hls;
pub mod state;

pub use engine::{category_of, download, job_from_url, Job, Limiter, Limits};
pub use events::Progress;
