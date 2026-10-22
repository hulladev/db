use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Per-host admission policy. Distributed storage limits live in PostgreSQL;
/// these counters are intentionally process-local and reset on restart.
#[derive(Debug, Clone, Copy)]
pub struct PrincipalQuotas {
    pub requests_per_minute: u32,
    pub max_inflight: usize,
    pub max_subscriptions: usize,
    pub max_identities: usize,
}
impl PrincipalQuotas {
    pub(super) fn valid(self) -> bool {
        self.requests_per_minute > 0
            && self.max_inflight > 0
            && self.max_subscriptions > 0
            && self.max_identities > 0
    }
}
type Key = (Uuid, Uuid);
struct Entry {
    window: Instant,
    requests: u32,
    inflight: usize,
    subscriptions: usize,
}
pub(super) struct Gate {
    options: PrincipalQuotas,
    entries: Mutex<BTreeMap<Key, Entry>>,
}
pub(super) struct Permit {
    gate: Arc<Gate>,
    key: Key,
    stream: bool,
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.gate.entries.lock()
            && let Some(entry) = entries.get_mut(&self.key)
        {
            if self.stream {
                entry.subscriptions -= 1;
            } else {
                entry.inflight -= 1;
            }
        }
    }
}
impl Gate {
    pub fn new(options: PrincipalQuotas) -> Arc<Self> {
        Arc::new(Self {
            options,
            entries: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn acquire(self: &Arc<Self>, key: Key, stream: bool) -> Option<Permit> {
        let now = Instant::now();
        let mut entries = self.entries.lock().ok()?;
        if !entries.contains_key(&key) {
            entries.retain(|_, entry| {
                entry.inflight != 0
                    || entry.subscriptions != 0
                    || now.duration_since(entry.window) < Duration::from_secs(60)
            });
            if entries.len() >= self.options.max_identities {
                return None;
            }
            entries.insert(
                key,
                Entry {
                    window: now,
                    requests: 0,
                    inflight: 0,
                    subscriptions: 0,
                },
            );
        }
        let entry = entries.get_mut(&key)?;
        if stream {
            if entry.subscriptions >= self.options.max_subscriptions {
                return None;
            }
            entry.subscriptions += 1;
        } else {
            if now.duration_since(entry.window) >= Duration::from_secs(60) {
                entry.window = now;
                entry.requests = 0;
            }
            if entry.requests >= self.options.requests_per_minute
                || entry.inflight >= self.options.max_inflight
            {
                return None;
            }
            entry.requests += 1;
            entry.inflight += 1;
        }
        Some(Permit {
            gate: self.clone(),
            key,
            stream,
        })
    }
}

#[cfg(test)]
#[path = "../../tests/unit/managed_quotas.rs"]
mod tests;
