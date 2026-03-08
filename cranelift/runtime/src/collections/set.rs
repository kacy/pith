//! Set[T] - unique element collection

use hashbrown::HashSet;
use std::hash::Hash;

pub struct ForgeSet<T> {
    inner: HashSet<T>,
}

impl<T: Eq + Hash> ForgeSet<T> {
    pub fn new() -> Self {
        ForgeSet { inner: HashSet::new() }
    }
    
    pub fn insert(&mut self, value: T) -> bool {
        self.inner.insert(value)
    }
    
    pub fn contains(&self, value: &T) -> bool {
        self.inner.contains(value)
    }
}
