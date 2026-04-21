use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Acquire a read guard, recovering the inner state if the lock is poisoned.
pub fn rw_read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

/// Acquire a write guard, recovering the inner state if the lock is poisoned.
pub fn rw_write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Acquire a mutex guard, recovering the inner state if the lock is poisoned.
pub fn mutex_lock<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn mutex_lock_recovers_after_poison() {
        let lock = Arc::new(Mutex::new(vec![1]));
        let worker_lock = Arc::clone(&lock);

        let result = thread::spawn(move || {
            let mut guard = worker_lock.lock().unwrap();
            guard.push(2);
            panic!("poison mutex");
        })
        .join();
        assert!(result.is_err());

        let mut guard = mutex_lock(&lock);
        guard.push(3);
        assert_eq!(*guard, vec![1, 2, 3]);
    }

    #[test]
    fn rw_read_recovers_after_poison() {
        let lock = Arc::new(RwLock::new(vec![1]));
        let worker_lock = Arc::clone(&lock);

        let result = thread::spawn(move || {
            let mut guard = worker_lock.write().unwrap();
            guard.push(2);
            panic!("poison rwlock");
        })
        .join();
        assert!(result.is_err());

        let guard = rw_read(&lock);
        assert_eq!(*guard, vec![1, 2]);
    }

    #[test]
    fn rw_write_recovers_after_poison() {
        let lock = Arc::new(RwLock::new(vec![1]));
        let worker_lock = Arc::clone(&lock);

        let result = thread::spawn(move || {
            let mut guard = worker_lock.write().unwrap();
            guard.push(2);
            panic!("poison rwlock");
        })
        .join();
        assert!(result.is_err());

        let mut guard = rw_write(&lock);
        guard.push(3);
        assert_eq!(*guard, vec![1, 2, 3]);
    }
}
