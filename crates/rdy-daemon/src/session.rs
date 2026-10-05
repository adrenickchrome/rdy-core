//! The table of live sessions (connections past their hello).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Proto {
    Ctl,
    Mcp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub proto: Proto,
    pub version: String,
    pub cwd: PathBuf,
    pub peer_uid: u32,
    pub connected_at_ms: u64,
}

struct Inner {
    map: Mutex<BTreeMap<SessionId, SessionInfo>>,
    next: AtomicU64,
}

/// Cheap to clone; clones share one table and one id counter.
#[derive(Clone)]
pub struct Sessions(Arc<Inner>);

impl Default for Sessions {
    fn default() -> Self {
        Sessions(Arc::new(Inner {
            map: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
        }))
    }
}

impl Sessions {
    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<SessionId, SessionInfo>> {
        self.0.map.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Adds a session under the next id (never reused in this table's life).
    pub fn insert(
        &self,
        proto: Proto,
        version: String,
        cwd: PathBuf,
        peer_uid: u32,
    ) -> SessionInfo {
        let id = SessionId(self.0.next.fetch_add(1, Ordering::Relaxed));
        let connected_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let info = SessionInfo {
            id,
            proto,
            version,
            cwd,
            peer_uid,
            connected_at_ms,
        };
        self.map().insert(id, info.clone());
        info
    }

    pub fn remove(&self, id: SessionId) -> Option<SessionInfo> {
        self.map().remove(&id)
    }

    pub fn get(&self, id: SessionId) -> Option<SessionInfo> {
        self.map().get(&id).cloned()
    }

    /// Ascending by id.
    pub fn list(&self) -> Vec<SessionInfo> {
        self.map().values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.map().len()
    }

    pub fn is_empty(&self) -> bool {
        self.map().is_empty()
    }
}
