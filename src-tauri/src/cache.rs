use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::model::UsageRecord;

/// Incremental cache keyed by (path, mtime, size) so unchanged session files
/// are not re-parsed on every refresh.
#[derive(Default)]
pub struct FileCache {
    entries: HashMap<PathBuf, (SystemTime, u64, Vec<UsageRecord>)>,
}

impl FileCache {
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn get(&self, path: &PathBuf, mtime: SystemTime, size: u64) -> Option<Vec<UsageRecord>> {
        self.entries
            .get(path)
            .filter(|(m, s, _)| *m == mtime && *s == size)
            .map(|(_, _, records)| records.clone())
    }

    pub fn insert(&mut self, path: PathBuf, mtime: SystemTime, size: u64, records: Vec<UsageRecord>) {
        self.entries.insert(path, (mtime, size, records));
    }
}
