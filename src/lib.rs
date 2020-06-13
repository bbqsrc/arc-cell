#![feature(integer_atomics)]

use std::sync::{
    atomic::{AtomicU128, Ordering},
};
use std::{marker::PhantomData, ptr::NonNull, fmt::{Debug, Display}, ops::Deref};


#[repr(transparent)]
pub struct ArcRef<T> {
    ptr: NonNull<ArcCellInner<T>>,
    phantom: PhantomData<ArcCellInner<T>>,
}

pub struct ArcCell<T> {
    ptr: NonNull<ArcCellInner<T>>,
    phantom: PhantomData<ArcCellInner<T>>,
}

#[repr(transparent)]
/// (strong: u64, ptr: u64)
struct ArcCellInner<T: ?Sized>(AtomicU128, PhantomData<T>);

unsafe impl<T: Sync + Send> Send for ArcCell<T> {}
unsafe impl<T: Sync + Send> Sync for ArcCell<T> {}

impl<T> ArcCellInner<T> {
    const MASK_STRONG: u128 = 0xFFFF_FFFF_FFFF_FFFF_0000_0000_0000_0000;
    const MASK_PTR: u128 = 0x0000_0000_0000_0000_FFFF_FFFF_FFFF_FFFF;

    const ONE_STRONG: u128 = 1 << 64;

    #[inline(always)]
    fn strong_count(&self) -> u64 {
        (self.0.load(Ordering::Acquire) & Self::MASK_STRONG >> 64) as u64
    }

    #[inline(always)]
    fn ptr(&self) -> *const T {
        (self.0.load(Ordering::Acquire) & Self::MASK_PTR) as *const T
    }

    #[inline(always)]
    fn set_ptr_null(&self) -> (u64, *mut T) {
        loop {
            let current = self.0.load(Ordering::Relaxed);
            let new = current & !Self::MASK_PTR;

            if let Ok(value) =
                self.0
                    .compare_exchange(current, new, Ordering::Release, Ordering::Relaxed)
            {
                let strong = (value & Self::MASK_STRONG >> 64) as u64;
                let ptr = (value & Self::MASK_PTR) as *mut T;

                return (strong, ptr);
            }
        }
    }

    #[inline(always)]
    fn new(ptr: *const T) -> ArcCellInner<T> {
        let start = Self::ONE_STRONG | (ptr as u128);
        // println!("-- Init");
        Self(AtomicU128::new(start), PhantomData::<T>)
    }

    #[inline(always)]
    fn increment_strong(&self) {
        self.0.fetch_add(Self::ONE_STRONG, Ordering::Release);
        // println!("-- Increment strong");
    }

    #[inline(always)]
    fn decrement_strong(&self) -> u32 {
        loop {
            let current = self.0.load(Ordering::Relaxed);
            let mut strong = (current & Self::MASK_STRONG) >> 64;
            strong -= 1;
            let new = (current & !Self::MASK_STRONG) | (strong << 64);

            if self
                .0
                .compare_exchange(current, new, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                // println!("-- Decrement strong");
                return strong as u32;
            }
        }
    }

    #[inline(always)]
    fn set_ptr(&self, ptr: *mut T) -> *mut T {
        loop {
            let current = self.0.load(Ordering::Relaxed);
            let new = (current & !Self::MASK_PTR) | ptr as u128;

            if let Ok(value) =
                self.0
                    .compare_exchange(current, new, Ordering::Release, Ordering::Relaxed)
            {
                // println!("-- Set ptr");
                return (value & Self::MASK_PTR) as usize as *mut T;
            }
        }
    }
}

impl<T> ArcCell<T> {
    fn from_inner(ptr: NonNull<ArcCellInner<T>>) -> Self {
        Self {
            ptr,
            phantom: PhantomData,
        }
    }
}

impl<T> Drop for ArcCell<T> {
    fn drop(&mut self) {
        if self.inner().decrement_strong() > 0 {
            return;
        }

        // Synchronise and drop
        let (_, ptr) = self.inner().set_ptr_null();
        // println!("-- Dropping {:x}", ptr as usize);

        drop(unsafe { Box::from_raw(ptr) });

        // We can deallocate the inner pointer now
        // println!("-- Dropping inner");
        unsafe {
            Box::from_raw(self.ptr.as_ptr());
        }
    }
}

impl<T> ArcCell<T> {
    #[inline]
    pub fn new(data: Box<T>) -> ArcCell<T> {
        let x = Box::new(ArcCellInner::new(Box::into_raw(data)));

        Self::from_inner(unsafe { NonNull::new_unchecked(Box::into_raw(x) as *mut _) })
    }

    /// Returns old data
    #[inline]
    pub fn set(&self, data: Box<T>) -> Box<T> {
        let old_ptr = self.inner().set_ptr(Box::into_raw(data));
        unsafe { Box::from_raw(old_ptr) }
    }

    #[inline]
    pub fn get(&self) -> ArcRef<T> {
        self.inner().increment_strong();
        ArcRef { ptr: self.ptr, phantom: self.phantom }
    }

    #[inline]
    pub fn strong_count(&self) -> u64 {
        self.inner().strong_count()
    }

    #[inline]
    fn inner(&self) -> &ArcCellInner<T> {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> ArcRef<T> {
    #[inline]
    fn inner(&self) -> &ArcCellInner<T> {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: Display> Display for ArcRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(unsafe { &*self.inner().ptr() }, f)
    }
}

impl<T: Debug> Debug for ArcRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(unsafe { &*self.inner().ptr() }, f)
    }
}

impl<T> Deref for ArcRef<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.inner().ptr() }
    }
}

impl<T> Clone for ArcCell<T> {
    fn clone(&self) -> Self {
        self.inner().increment_strong();

        Self {
            ptr: self.ptr,
            phantom: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_test() {
        let v = ArcCell::new(Box::new("Something horrible".to_string()));
        let v0 = ArcCell::clone(&v);
        let t = std::thread::spawn(move || {
            println!("{}", v0.get());
            let v1 = ArcCell::clone(&v0);
            let _t = std::thread::spawn(move || {
                println!("{}", v1.get());
                v1.set(Box::new("Some other value again heh".to_string()));
            });
            v0.set(Box::new("Some other value".to_string()));
        });
        let v0 = ArcCell::clone(&v);
        let _t = std::thread::spawn(move || {
            println!("{}", v0.get());
            let v1 = ArcCell::clone(&v0);
            let _t = std::thread::spawn(move || {
                println!("{}", v1.get());
                v1.set(Box::new("Some other value again heh 1".to_string()));
            });
            v0.set(Box::new("Some other value".to_string()));
        });
        let v0 = ArcCell::clone(&v);
        let _t = std::thread::spawn(move || {
            println!("{}", v0.get());
            let v1 = ArcCell::clone(&v0);
            let _t = std::thread::spawn(move || {
                println!("{}", v1.get());
                v1.set(Box::new("Some other value again heh 2".to_string()));
            });
            v0.set(Box::new("Some other value".to_string()));
        });
        let v0 = ArcCell::clone(&v);
        let _t = std::thread::spawn(move || {
            println!("{}", v0.get());
            let v1 = ArcCell::clone(&v0);
            let _t = std::thread::spawn(move || {
                println!("{}", v1.get());
                v1.set(Box::new("Some other value again heh 3".to_string()));
            });
            v0.set(Box::new("Some other value".to_string()));
        });
        let v0 = ArcCell::clone(&v);
        let _t = std::thread::spawn(move || {
            println!("{}", v0.get());
            let v1 = ArcCell::clone(&v0);
            let _t = std::thread::spawn(move || {
                println!("{}", v1.get());
                v1.set(Box::new("Some other value again heh 4".to_string()));
            });
            v0.set(Box::new("Some other value".to_string()));
        });
        let v0 = ArcCell::clone(&v);
        let _t = std::thread::spawn(move || {
            println!("{}", v0.get());
            let v1 = ArcCell::clone(&v0);
            let _t = std::thread::spawn(move || {
                println!("{}", v1.get());
                v1.set(Box::new("Some other value again heh 5".to_string()));
            });
            v0.set(Box::new("Some other value".to_string()));
        });
        let v0 = ArcCell::clone(&v);
        let _t = std::thread::spawn(move || {
            println!("{}", v0.get());
            let v1 = ArcCell::clone(&v0);
            let _t = std::thread::spawn(move || {
                println!("{}", v1.get());
                v1.set(Box::new("Some other value again heh 6".to_string()));
            });
            v0.set(Box::new("Some other value".to_string()));
        });
        println!("A: {}", v.get());
        let _ = t.join();
        println!("B: {}", v.get());
    }
}
