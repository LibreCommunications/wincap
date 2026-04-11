use crossbeam_utils::CachePadded;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Lock-free single-producer / single-consumer bounded ring buffer.
/// Capacity must be a power of two (enforced at compile time via const assert).
///
/// Memory ordering follows the canonical Vyukov SPSC pattern:
///   producer: load tail (acquire), store head (release)
///   consumer: load head (acquire), store tail (release)
pub struct SpscRing<T, const N: usize> {
    slots: Box<[UnsafeCell<Option<T>>]>,
    head: CachePadded<AtomicUsize>, // producer
    tail: CachePadded<AtomicUsize>, // consumer
}

// SAFETY: SpscRing is designed for exactly one producer thread and one consumer thread.
// The atomic ordering guarantees correctness for that pattern.
unsafe impl<T: Send, const N: usize> Send for SpscRing<T, N> {}
unsafe impl<T: Send, const N: usize> Sync for SpscRing<T, N> {}

impl<T, const N: usize> SpscRing<T, N> {
    const MASK: usize = N - 1;

    pub fn new() -> Self {
        // Compile-time check: N must be a power of two and >= 2.
        const { assert!(N >= 2 && (N & (N - 1)) == 0, "Capacity must be a power of two >= 2") };

        let mut slots = Vec::with_capacity(N);
        for _ in 0..N {
            slots.push(UnsafeCell::new(None));
        }
        Self {
            slots: slots.into_boxed_slice(),
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
        }
    }

    /// Producer: try to push. Returns `Err(value)` if full.
    pub fn try_push(&self, value: T) -> Result<(), T> {
        let head = self.head.load(Ordering::Relaxed);
        let next = (head + 1) & Self::MASK;
        if next == self.tail.load(Ordering::Acquire) {
            return Err(value);
        }
        unsafe { *self.slots[head].get() = Some(value) };
        self.head.store(next, Ordering::Release);
        Ok(())
    }

    /// Producer: push, evicting the oldest entry if full.
    /// Returns `Some(evicted)` if an element was dropped.
    pub fn push_overwrite(&self, value: T) -> Option<T> {
        match self.try_push(value) {
            Ok(()) => None,
            Err(value) => {
                let evicted = self.try_pop();
                let _ = self.try_push(value);
                evicted
            }
        }
    }

    /// Consumer: try to pop. Returns `None` if empty.
    pub fn try_pop(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.head.load(Ordering::Acquire) {
            return None;
        }
        let value = unsafe { (*self.slots[tail].get()).take() };
        let next = (tail + 1) & Self::MASK;
        self.tail.store(next, Ordering::Release);
        value
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) == self.tail.load(Ordering::Acquire)
    }
}

impl<T, const N: usize> Default for SpscRing<T, N> {
    fn default() -> Self {
        Self::new()
    }
}
