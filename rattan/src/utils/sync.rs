use std::{
    marker::PhantomData,
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
};

pub struct AtomicRawCell<T> {
    ptr: AtomicPtr<T>,
    /// This effectively makes `AtomicRawCell<T>` non-`Send` and non-`Sync` if `T`
    /// is non-`Send`.
    phantom: PhantomData<Box<T>>,
}

/// Mark `AtomicRawCell<T>` as safe to share across threads.
///
/// This is safe because shared access to an `AtomicRawCell<T>` does not provide
/// shared access to any `T` value. However, it does provide the ability to get
/// a `Box<T>` from another thread, so `T: Send` is required.
unsafe impl<T> Sync for AtomicRawCell<T> where T: Send {}

impl<T> AtomicRawCell<T> {
    pub fn new(initial: Box<T>) -> AtomicRawCell<T> {
        AtomicRawCell {
            ptr: AtomicPtr::new(Box::into_raw(initial)),
            phantom: PhantomData,
        }
    }

    fn swap_raw(&self, ptr: *mut T, order: Ordering) -> *mut T {
        self.ptr.swap(ptr, order)
    }

    fn swap_raw_boxed(&self, ptr: *mut T) -> Option<Box<T>> {
        let old_ptr = self.swap_raw(ptr, Ordering::AcqRel);
        if old_ptr.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(old_ptr) })
        }
    }

    pub fn swap(&self, new: Box<T>) -> Option<Box<T>> {
        let new_ptr = Box::into_raw(new);
        let old_ptr = self.ptr.swap(new_ptr, Ordering::AcqRel);
        if old_ptr.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(old_ptr) })
        }
    }

    pub fn swap_null(&self) -> Option<Box<T>> {
        self.swap_raw_boxed(std::ptr::null_mut())
    }

    /// Returns a mutable reference to the contained value.
    ///
    /// This is safe because it borrows the `AtomicRawCell` mutably, which ensures
    /// that no other threads can concurrently access either the atomic pointer field
    /// or the boxed data it points to.
    pub fn get_mut(&mut self) -> &mut T {
        // Relaxed suffices here because this thread must already have
        // rendezvoused with any other thread that's been modifying shared
        // data, and executed an Acquire barrier, in order for the caller to
        // have a `mut` reference.  Symmetrically, no barrier is needed when
        // the reference expires, because this thread must rendezvous with
        // other threads, and execute a Release barrier, before this AtomicRawCell
        // becomes shared again.
        let ptr = self.ptr.load(Ordering::Relaxed);
        unsafe { &mut *ptr }
    }

    pub fn store(&self, new: Box<T>) {
        let new_ptr = Box::into_raw(new);
        self.ptr.store(new_ptr, Ordering::Release);
    }
}

impl<T> Drop for AtomicRawCell<T> {
    fn drop(&mut self) {
        let inner_ptr = self.ptr.load(Ordering::Relaxed);
        if !inner_ptr.is_null() {
            let _ = unsafe { Box::from_raw(inner_ptr) };
        }
    }
}

pub struct AtomicF64 {
    storage: AtomicU64,
}

impl AtomicF64 {
    pub fn new(value: f64) -> Self {
        let as_u64 = value.to_bits();
        Self {
            storage: AtomicU64::new(as_u64),
        }
    }
    pub fn store(&self, value: f64, ordering: Ordering) {
        let as_u64 = value.to_bits();
        self.storage.store(as_u64, ordering)
    }
    pub fn load(&self, ordering: Ordering) -> f64 {
        let as_u64 = self.storage.load(ordering);
        f64::from_bits(as_u64)
    }
}
