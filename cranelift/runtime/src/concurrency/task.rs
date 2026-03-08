//! Task system for spawn/await

pub struct Task<T> {
    result: Option<T>,
}

impl<T> Task<T> {
    pub fn new() -> Self {
        Task { result: None }
    }
}
