//! In-memory cookie sessions with TTL and a hard cap.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct Session {
    pub visits: u64,
    pub data: HashMap<String, String>,
    last: Instant,
}

pub struct SessionStore {
    map: HashMap<String, Session>,
    ttl: Duration,
    max: usize,
    counter: u64,
}

impl SessionStore {
    pub fn new(ttl: Duration, max: usize) -> Self {
        Self {
            map: HashMap::new(),
            ttl,
            max,
            counter: 0,
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True if the id exists and has not expired; refreshes its timestamp.
    pub fn is_valid(&mut self, id: &str) -> bool {
        let ttl = self.ttl;
        let state = self.map.get(id).map(|s| s.last.elapsed() < ttl);
        match state {
            Some(true) => {
                if let Some(s) = self.map.get_mut(id) {
                    s.last = Instant::now();
                }
                true
            }
            Some(false) => {
                self.map.remove(id);
                false
            }
            None => false,
        }
    }

    /// Creates a new session. Returns None when the store is full.
    pub fn create(&mut self) -> Option<String> {
        if self.map.len() >= self.max {
            self.purge();
            if self.map.len() >= self.max {
                return None;
            }
        }
        let mut id = self.random_hex();
        while self.map.contains_key(&id) {
            id = self.random_hex();
        }
        self.map.insert(
            id.clone(),
            Session {
                visits: 0,
                data: HashMap::new(),
                last: Instant::now(),
            },
        );
        Some(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.map.get_mut(id)
    }

    pub fn remove(&mut self, id: &str) {
        self.map.remove(id);
    }

    pub fn purge(&mut self) {
        let ttl = self.ttl;
        self.map.retain(|_, s| s.last.elapsed() < ttl);
    }

    fn random_hex(&mut self) -> String {
        let mut b = [0u8; 16];
        let ok = std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut b))
            .is_ok();
        if !ok {
            // Fallback: not cryptographically strong, only used if /dev/urandom is unavailable.
            self.counter = self.counter.wrapping_add(1);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mut h1 = DefaultHasher::new();
            (nanos, self.counter, 1u8).hash(&mut h1);
            let mut h2 = DefaultHasher::new();
            (nanos, self.counter, 2u8).hash(&mut h2);
            b[..8].copy_from_slice(&h1.finish().to_le_bytes());
            b[8..].copy_from_slice(&h2.finish().to_le_bytes());
        }
        b.iter().map(|x| format!("{:02x}", x)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_validate() {
        let mut s = SessionStore::new(Duration::from_secs(60), 10);
        let id = s.create().unwrap();
        assert_eq!(id.len(), 32);
        assert!(s.is_valid(&id));
        assert!(!s.is_valid("nope"));
        s.get_mut(&id).unwrap().visits += 1;
        assert_eq!(s.get_mut(&id).unwrap().visits, 1);
        s.remove(&id);
        assert!(!s.is_valid(&id));
    }

    #[test]
    fn ids_are_unique() {
        let mut s = SessionStore::new(Duration::from_secs(60), 100);
        let a = s.create().unwrap();
        let b = s.create().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn expiry_and_cap() {
        let mut s = SessionStore::new(Duration::from_millis(0), 2);
        let id = s.create().unwrap();
        assert!(!s.is_valid(&id)); // ttl 0 -> instantly expired
        let mut s = SessionStore::new(Duration::from_secs(60), 2);
        s.create().unwrap();
        s.create().unwrap();
        assert!(s.create().is_none());
        assert_eq!(s.len(), 2);
    }
}
