use serde::{Deserialize, Serialize};

/// One range segment: [start, end] inclusive, `done` bytes already written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub start: u64,
    pub end: u64,
    pub done: u64,
}

impl Segment {
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }
    pub fn remaining(&self) -> u64 {
        self.len() - self.done
    }
    pub fn complete(&self) -> bool {
        self.done >= self.len()
    }
    /// Absolute file offset of the next byte to fetch.
    pub fn offset(&self) -> u64 {
        self.start + self.done
    }
}

/// Persistent job state, stored next to the target as `<name>.rdm-part.json`.
/// Removed on successful completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobState {
    pub url: String,
    pub size: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub segments: Vec<Segment>,
}

impl JobState {
    pub fn total_len(&self) -> Option<u64> {
        self.size
    }

    pub fn total_done(&self) -> u64 {
        self.segments.iter().map(|s| s.done).sum()
    }

    pub fn complete(&self) -> bool {
        self.size.map_or(false, |sz| self.total_done() == sz)
            && self.segments.iter().all(|s| s.complete())
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Option<Self>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// Split `size` bytes into at most `segments` balanced inclusive ranges.
pub fn split(size: u64, segments: u64) -> Vec<Segment> {
    let n = segments.max(1).min(size.max(1));
    let base = size / n;
    let rem = size % n;
    let mut out = Vec::with_capacity(n as usize);
    let mut start = 0u64;
    for i in 0..n {
        let extra = if i < rem { 1 } else { 0 };
        let end = start + base + extra - 1;
        out.push(Segment { start, end, done: 0 });
        start = end + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_covers_exactly() {
        for (size, n) in [(100, 4), (7, 3), (1, 8), (0, 4), (10_000_000, 8)] {
            if size == 0 {
                continue;
            }
            let segs = split(size, n);
            assert_eq!(segs.first().unwrap().start, 0);
            assert_eq!(segs.last().unwrap().end, size - 1);
            let total: u64 = segs.iter().map(|s| s.len()).sum();
            assert_eq!(total, size, "size={size} n={n}");
            for w in segs.windows(2) {
                assert_eq!(w[0].end + 1, w[1].start);
            }
        }
    }
}
