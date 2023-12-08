use std::{
    fmt::{Debug, Formatter},
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
    vec::Vec as StdVec,
};

/// A "vector" that is implemented with a linked list.
///
/// # Random Access
/// A vector should support random access, but linked list cannot do this, this
/// is why the vector word is quoted.
pub struct LinkedListVec<T> {
    head: AtomicPtr<Node<T>>,
    tail: AtomicPtr<Node<T>>,
    len: AtomicUsize,
}

impl<T: Debug> Debug for LinkedListVec<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut vec: Vec<&Node<T>> = StdVec::new();

        // Take a snapshot on what is in the Vector, the snapshot is possibly
        // inaccurate as the structure support concurrent appends.
        // For example, it can have data `[1, 2]` but length 1.
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        let len = self.len.load(Ordering::Relaxed);

        if !head.is_null() {
            let mut p = head;

            loop {
                // SAFETY:
                //
                // The raw pointer `p` won't be
                //    1. NULL as
                //       * For the first loop, we just checked `!head.is_null()`
                //       * For the remaining loops, the loop stops at `tail` and
                //         all the pointers in-between come from `Box::into_raw()`
                //
                //    2. dangling as the written pointer comes from `Box::into_raw()`
                //       and for a written value, we won't modify it.
                let p_node = unsafe { &*p };
                vec.push(p_node);

                if std::ptr::eq(p, tail) {
                    break;
                }

                let p_next = p_node.next.load(Ordering::Relaxed);
                p = p_next;
            }
        }

        f.debug_struct("LinkedListVec")
            .field("array", &vec)
            .field("len", &len)
            .finish()
    }
}

impl<T> Drop for LinkedListVec<T> {
    fn drop(&mut self) {
        // When dropping, it is guaranteed that no one is accessing the vector,
        // so this snapshot is accurate.
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);

        if !head.is_null() {
            let mut p = head;

            loop {
                // SAFETY:
                // For pointers stored between `head` and `tail`, they all come
                // from `Box::into_raw()`, so it is safe to convert it back with
                // `Box::from_raw()`.
                let p_node = unsafe { Box::from_raw(p) };
                let p_next = p_node.next.load(Ordering::Relaxed);

                if std::ptr::eq(p, tail) {
                    break;
                }

                p = p_next;
            }
        }
    }
}

/// A node within the linked list.
struct Node<T> {
    data: T,
    next: AtomicPtr<Node<T>>,
}

impl<T: Debug> Debug for Node<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.data.fmt(f)
    }
}

impl<T> Node<T> {
    /// Create a [`Node`] whose `next` field is NULL.
    fn new(val: T) -> Self {
        Self {
            data: val,
            next: AtomicPtr::new(null_mut()),
        }
    }
}

impl<T> LinkedListVec<T> {
    /// Create an empty `LinkedListVec`.
    pub fn new() -> Self {
        Self {
            head: AtomicPtr::new(null_mut()),
            tail: AtomicPtr::new(null_mut()),
            len: AtomicUsize::new(0),
        }
    }

    /// Push an item to the vector.
    pub fn push(&self, val: T) {
        let node = Node::new(val);
        let node_ptr = Box::into_raw(Box::new(node));

        if self
            .tail
            .compare_exchange(
                null_mut(),
                node_ptr,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            self.head.store(node_ptr, Ordering::Relaxed);
        } else {
            loop {
                let snapshot = self.tail.load(Ordering::Relaxed);
                assert_ne!(snapshot, null_mut());

                if self
                    .tail
                    .compare_exchange(
                        snapshot,
                        node_ptr,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    // SAFETY:
                    // The raw pointer `snapshot` can not be
                    //   1. NULL as we have checked it `assert_ne!(snapshot, null_mut())`
                    //   2. dangling as it comes from `Box::into_raw()`
                    //   3. unaligned as it comes from `Box::into_raw()`
                    unsafe { &*snapshot }
                        .next
                        .store(node_ptr, Ordering::Relaxed);
                    break;
                }
            }
        }

        self.len.fetch_add(1, Ordering::Relaxed);
    }

    /// Get the value at the index `idx`.
    ///
    /// # Optimization
    ///
    /// Linked list can not do random access, which means to find an item, we
    /// have to iterate over the item before the requested one.
    ///
    /// Can we do some optimization here, say when `idx` equals to `len`, then
    /// we can just return the value at `self.tail`. Unfortunately, we can not
    /// do this as while loading the value of `len`, the `tail` field can be
    /// updated by other thread, which would return the wrong item.
    pub fn get(&self, idx: usize) -> Option<&T> {
        let len = self.len.load(Ordering::Relaxed);
        if idx >= len {
            return None;
        }

        let mut p = self.head.load(Ordering::Relaxed);
        for _ in 0..idx {
            // SAFETY:
            // The raw pointer `p` it not
            //   1. NULL
            //     * For first loop, `self.head` is not NULL as `len` is not 0
            //     * For remaining loops, `p` can not be NULL as these pointers
            //       come from `Box::from_raw()`
            //
            //   2. dangling as it comes from `Box::from_raw()`
            //   2. unaligned as it comes from `Box::from_raw()`
            p = unsafe { &*p }.next.load(Ordering::Relaxed);
        }

        // SAFETY:
        // Safe for the same reason as the previous `unsafe` block within this
        // function.
        Some(&unsafe { &*p }.data)
    }

    /// Return the length.
    ///
    /// It is inaccurate due to concurrent appends.
    #[inline]
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, thread::spawn};

    #[test]
    fn it_works() {
        let vec = Arc::new(LinkedListVec::new());
        let mut handles = Vec::new();

        for thread_id in 0..5 {
            handles.push(spawn({
                let vec = Arc::clone(&vec);

                move || {
                    for _ in 0..5 {
                        vec.push(thread_id);
                    }
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(vec.len(), 25);
        let mut counter = [0_usize; 5];
        for idx in 0..25 {
            let num = *vec.get(idx).unwrap();
            counter[num as usize] += 1;
        }

        for item in counter {
            assert_eq!(item, 5);
        }
    }
}
