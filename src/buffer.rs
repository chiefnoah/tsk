use std::{
    collections::BinaryHeap,
    sync::{Condvar, Mutex},
};
struct PooledHandle<'a, T: Ord + Default> {
    parent: &'a Pool<T>,
    item: Option<T>,
}

struct Pool<T: Ord + Default>(Mutex<BinaryHeap<T>>, Condvar);

impl<'a, T: Ord + Default> Pool<T> {
    pub fn give(&self, item: T) {
        self.0.lock().expect("Poisoned lock").push(item)
    }

    /// Retrieves
    pub fn take(&self) -> Option<T> {
        self.0.lock().expect("Poisoned lock").pop()
    }

    pub fn lease(&self) -> PooledHandle<'a, T> {
        let guard = self.0.lock().expect("Poisoned lock");
        match guard.pop() {
            Some(item) => PooledHandle {
                parent: self,
                item: Some(item),
            },
            None => {
                let mut item = None;
                while item.is_none() {
                    let guard = self.1.wait(guard).expect("Poisoned lock");
                    item = guard.pop();
                }

            },
        }
    }
}

impl<T: Ord> AsRef<T> for PooledHandle<'_, T> {
    fn as_ref(&self) -> &T {
        &self.item
    }
}

impl<T: Ord> Drop for PooledHandle<'_, T> {
    fn drop(&mut self) {
        self.parent.give(self.item)
    }
}
