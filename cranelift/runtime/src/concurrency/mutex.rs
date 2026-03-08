//! Mutex synchronization primitive

pub struct ForgeMutex<T> {
    inner: std::sync::Mutex<T>,
}

impl<T> ForgeMutex<T> {
    pub fn new(data: T) -> Self {
        ForgeMutex {
            inner: std::sync::Mutex::new(data),
        }
    }
}
