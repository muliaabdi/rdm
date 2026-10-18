use serde::{Deserialize, Serialize};

/// Engine progress stream for one job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Progress {
    Started {
        url: String,
        filename: String,
        size: Option<u64>,
        segments: usize,
    },
    Bytes {
        delta: u64,
        total_done: u64,
        size: Option<u64>,
    },
    SegmentDone {
        index: usize,
    },
    Retrying {
        index: usize,
        attempt: u32,
        error: String,
    },
    Completed {
        size: u64,
        sha256: Option<String>,
    },
    Failed {
        error: String,
    },
    Cancelled {
        total_done: u64,
    },
}

impl Progress {
    /// Human one-line form for CLI display.
    pub fn brief(&self) -> String {
        match self {
            Progress::Started { filename, size, segments, .. } => format!(
                "start {filename} {} seg={segments}",
                size.map(|s| format!("{s}B")).unwrap_or_else(|| "?B".into())
            ),
            Progress::Bytes { total_done, size, .. } => match size {
                Some(s) if *s > 0 => format!("\r{:.1}% {}/{}B", 100.0 * *total_done as f64 / *s as f64, total_done, s),
                _ => format!("\r{}B", total_done),
            },
            Progress::SegmentDone { index } => format!("segment {index} done"),
            Progress::Retrying { index, attempt, error } => {
                format!("retry seg {index} #{attempt}: {error}")
            }
            Progress::Completed { size, sha256 } => {
                format!("\ndone {size}B sha256={}", sha256.as_deref().unwrap_or("-"))
            }
            Progress::Failed { error } => format!("failed: {error}"),
            Progress::Cancelled { total_done } => format!("cancelled @{total_done}B (state saved)"),
        }
    }
}
