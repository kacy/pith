//! List[T] - ordered, contiguous array-backed list

/// List implementation using Vec
pub struct ForgeList<T> {
    items: Vec<T>,
}

impl<T> ForgeList<T> {
    pub fn new() -> Self {
        ForgeList { items: Vec::new() }
    }
    
    pub fn push(&mut self, item: T) {
        self.items.push(item);
    }
    
    pub fn len(&self) -> usize {
        self.items.len()
    }
}

// FFI exports will be added here
