// Taken from `lockless` (license https://github.com/Diggsey/lockless/blob/master/Cargo.toml#L7)
// and modified

use std::ops::Not;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::{mem, ptr};

type NodePtr<T> = Option<Box<Node<T>>>;

#[derive(Debug)]
struct Node<T> {
    value: T,
    next: AppendList<T>,
}

#[derive(Debug)]
pub struct AppendList<T>(AtomicPtr<Node<T>>);

impl<T> AppendList<T> {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self::new_internal(None)
    }

    pub fn append(&self, value: T) {
        self.append_list(AppendList::new_internal(Some(Box::new(Node {
            value,
            next: AppendList::new(),
        }))));
    }

    pub fn append_list(&self, other: AppendList<T>) {
        let p = other.0.load(Ordering::Acquire);
        mem::forget(other);
        unsafe { self.append_ptr(p) };
    }

    pub const fn iter(&self) -> AppendListIterator<T> {
        AppendListIterator(&self.0)
    }

    #[allow(clippy::wrong_self_convention)]
    fn into_raw(ptr: NodePtr<T>) -> *mut Node<T> {
        match ptr {
            Some(b) => Box::into_raw(b),
            None => ptr::null_mut(),
        }
    }

    unsafe fn from_raw(ptr: *mut Node<T>) -> NodePtr<T> {
        ptr.is_null().not().then(|| Box::from_raw(ptr))
    }

    fn new_internal(ptr: NodePtr<T>) -> Self {
        AppendList(AtomicPtr::new(Self::into_raw(ptr)))
    }

    unsafe fn append_ptr(&self, p: *mut Node<T>) {
        loop {
            match self.0.compare_exchange_weak(
                ptr::null_mut(),
                p,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(head) => {
                    if !head.is_null() {
                        return (*head).next.append_ptr(p);
                    }
                }
            }
        }
    }
}

impl<T> Drop for AppendList<T> {
    fn drop(&mut self) {
        unsafe { Self::from_raw(mem::replace(self.0.get_mut(), ptr::null_mut())) };
    }
}

impl<'a, T> IntoIterator for &'a AppendList<T> {
    type Item = &'a T;
    type IntoIter = AppendListIterator<'a, T>;

    fn into_iter(self) -> AppendListIterator<'a, T> {
        self.iter()
    }
}

#[derive(Debug)]
pub struct AppendListIterator<'a, T: 'a>(&'a AtomicPtr<Node<T>>);

impl<'a, T: 'a> Iterator for AppendListIterator<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        let p = self.0.load(Ordering::Acquire);
        p.is_null().not().then(|| unsafe {
            self.0 = &(*p).next.0;
            &(*p).value
        })
    }
}
