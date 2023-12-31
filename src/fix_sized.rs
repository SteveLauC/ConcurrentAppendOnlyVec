use std::{
    cell::UnsafeCell,
    fmt::{Debug, Formatter},
    mem::MaybeUninit,
    result::Result,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    vec::Vec as StdVec,
};

/// A fix-sized, lock-free, append-only Vector.
pub struct FixSizedVec<T, const N: usize> {
    /// The `AtomicBool` here is used to indicate whether this entry has been
    /// initialized or not, we cannot use `[Option<T>; N]` here as updating a
    /// `Option` is not atomic, when reading it with the `get()` method, partially
    /// initialized memory can be read and causes UB.
    ///
    /// There is indeed an `AtomicOption` crate, but it is basically equivalent
    /// to using an `AtomicBool` flag.
    array: UnsafeCell<[(MaybeUninit<T>, AtomicBool); N]>,
    /// Length
    ///
    /// It also controls which index a thread will write to.
    len: AtomicUsize,
}

impl<T: Debug, const N: usize> Debug for FixSizedVec<T, N> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut array = StdVec::with_capacity(N);
        // SAFETY:
        // We only read the array, and we won't read a piece of memory what is
        // in intermediate state as guarded by that `AtomicBool` flag.
        for (val, inited) in unsafe { &*self.array.get() } {
            if inited.load(Ordering::Relaxed) {
                // SAFETY:
                // It is guaranteed to be initialized as the `inited` flag is true.
                array.push(Some(unsafe { val.assume_init_ref() }));
            } else {
                array.push(None);
            }
        }

        f.debug_struct("FixSizedVec")
            .field("array", &array)
            .field("len", &self.len)
            .finish()
    }
}

// SAFETY:
//
// It is synchronized:
//
// * For write:
//
//   With `UnsafeCell` we can write to the inner value even with an immutable
//   reference, but multiple threads will write to the same memory.
//
// * For read:
//
//   1. Even though the value stored in `MaybeUninit<T>` can be partially initialized,
//      we won't read it cause we will check the `AtomicBool` flag first before we
//      access the value.
//
//      It can be seen as `AtomicBool<MaybeUninit<T>>`
//
//   2. A written value won't be changed so that we can safely read an item.
unsafe impl<T, const N: usize> Sync for FixSizedVec<T, N> {}

impl<T, const N: usize> FixSizedVec<T, N> {
    /// Create an empty vector.
    pub fn new() -> Self {
        let array = std::array::from_fn(|_| {
            (MaybeUninit::uninit(), AtomicBool::new(false))
        });
        Self {
            array: UnsafeCell::new(array),
            len: AtomicUsize::new(0),
        }
    }

    /// Push an item to it
    ///
    /// Return `Ok(())` when it is successfully written, `Err(())` when the vector is full.
    #[allow(clippy::result_unit_err)] // using unit as the err type is ok for demo code
    pub fn push(&self, val: T) -> Result<(), ()> {
        loop {
            let snapshot = self.len.load(Ordering::Relaxed);
            if snapshot == N {
                return Err(());
            }

            if self
                .len
                .compare_exchange(
                    snapshot,
                    snapshot + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                let ptr = self.array.get();

                // SAFETY:
                //
                // It is safe because
                //     1. the raw pointer `ptr` comes from `&self` so that it
                //        cannot be NULL or dangling.
                //     2. `snapshot` is guaranteed to be valid index as it has
                //        already been checked.
                let (entry, inited) =
                    unsafe { (*ptr).get_unchecked_mut(snapshot) };

                assert!(!inited.load(Ordering::Relaxed));
                entry.write(val);
                inited.store(true, Ordering::Relaxed);

                return Ok(());
            }
        }
    }

    /// Get the value at `idx`
    ///
    /// For uninitialized value, a `None` is returned. Otherwise, return an
    /// reference to the value.
    ///
    // We don't need to worry that the value will be modified while holding
    // the returned reference as the stored value won't be modified at all.
    pub fn get(&self, idx: usize) -> Option<&T> {
        if idx >= N {
            return None;
        }

        let p = self.array.get();

        // SAFETY:
        // The pointer `p` cannot be dangling or NULL as it comes from `&self`
        let (val, inited) = &unsafe { &*p }[idx];

        if inited.load(Ordering::Relaxed) {
            // SAFETY:
            // It is guaranteed to be initialized as the `inited` flag is true.
            Some(unsafe { val.assume_init_ref() })
        } else {
            None
        }
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
        let vec = Arc::new(FixSizedVec::<i32, 25>::new());
        let mut handles = Vec::new();

        for thread_id in 0..5 {
            handles.push(spawn({
                let vec = Arc::clone(&vec);

                move || {
                    for _ in 0..5 {
                        vec.push(thread_id).unwrap();
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
