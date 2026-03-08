//! Map[K,V] - hash-indexed key-value collection

use hashbrown::HashMap;
use std::hash::Hash;

pub struct ForgeMap<K, V> {
    inner: HashMap<K, V>,
}

impl<K: Eq + Hash, V> ForgeMap<K, V> {
    pub fn new() -> Self {
        ForgeMap { inner: HashMap::new() }
    }
    
    pub fn insert(&mut self, key: K, value: V) {
        self.inner.insert(key, value);
    }
    
    pub fn get(&self, key: &K) -> Option<&V> {
        self.inner.get(key)
    }
}
