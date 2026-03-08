//! Channel[T] for inter-task communication

pub struct Channel<T> {
    // TODO: Implement with crossbeam or std::sync::mpsc
    _phantom: std::marker::PhantomData<T>,
}
