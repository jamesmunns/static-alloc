//! Reference counter value.
//!
//!
use core::{mem, ptr};
use core::alloc::Layout;
use core::cell::Cell;

use crate::uninit::{Uninit, UninitView};

pub struct Rc<'a, T> {
    /// Shared view on the memory of the box.
    ///
    /// It is important **NOT** to safely expose this to the user. The weak counter maintains the
    /// invariant that the pointed-to memory is no longer aliased when the last Rc to that view has
    /// been dropped.
    inner: UninitView<'a, RcBox<T>>,
}

/// A structured container for the boxed value.
///
/// It's representation is chosen such that it can be cast to `Uninit<T>` and from it given
/// appropriate additional space. All added data is at the end of the allocation, this allows other
/// containers that store the value to reuse the same allocation without shoveling data around.
///
/// That however, is an implementation detail since we could also `memmove` appropriately. And it
/// falls apart as soon as we take extra alignment requirements into account. Hence, we do not
/// expose it generally and give no guarantees outside the basic conversion. Make this
/// incrementally better.
#[repr(C)]
struct RcBox<T> {
    /// Keep this member first!
    val: T,

    /// The number of owners of the value.
    strong: Cell<usize>,

    /// The number of owners of the memory view.
    ///
    /// Note that the strong ownership of the value also counts as a *single* weak ownership. The
    /// last access which drops the value should also decrease the weak count.
    weak: Cell<usize>,
}

impl<'a, T> Rc<'a, T> {
    pub fn new(val: T, memory: Uninit<'a, ()>) -> Self {
        assert!(memory.fits(Self::layout()), "Provided memory must fit the inner layout");
        let mut memory = memory.cast::<RcBox<T>>().unwrap();

        memory.borrow_mut().init(RcBox {
            val,
            strong: Cell::new(1),
            weak: Cell::new(1),
        });

        Rc {
            inner: memory.into(),
        }
    }

    pub unsafe fn from_raw(init: Uninit<'a, T>) -> Self {
        // TODO: offset from the val field.
        unimplemented!()
    }

    /// Try to extract the memory.
    ///
    /// This returns `Some` only when this is the last strong *and* weak reference to the value.
    /// The contained value will be preserved and is not dropped. Use `from_raw` to reinitialize a
    /// new `Rc` with the old value and memory.
    ///
    /// ## Example
    ///
    /// ```
    /// use static_alloc::{Slab, rc::Rc};
    ///
    /// struct HotPotato;
    ///
    /// impl Drop for HotPotato {
    ///     fn drop(&mut self) {
    ///         panic!("dropped!");
    ///     }
    /// }
    ///
    /// let slab: Slab<[u8; 1024]> = Slab::uninit();
    /// let foo = slab.rc(HotPotato).unwrap();
    ///
    /// let raw = Rc::into_raw(foo).unwrap();
    /// // No panic. Value has not been dropped.
    /// ```
    pub fn into_raw(rc: Self) -> Option<Uninit<'a, T>> {
        if Rc::weak_count(&rc) != 1 && Rc::strong_count(&rc) != 1 {
            // Note: implicitely decrements `strong`
            return None;
        }

        let ptr = rc.inner.as_non_null();
        let len = rc.inner.size();
        mem::forget(rc);
        unsafe {
            // SAFETY: restored the memory we just forgot. We are the only reference to it, so it
            // is fine to restore the original unqiue allocation reference.
            Some(Uninit::from_memory(ptr.cast(), len).cast().unwrap())
        }
    }

    pub fn try_unwrap(b: Self) -> Result<(T, Uninit<'a, T>), Self> {
        unimplemented!()
    }
}

impl<T> Rc<'_, T> {
    /// Get the layout that needs to be allocated to create the inner structure of an `Rc`.
    ///
    /// You should not rely on the value returned here. The only guarantee is that the size of the
    /// layout is at least as large as the input type.
    ///
    /// An `Rc` does not simply point to a lone instance of a type but instead adds some small
    /// metadata (two pointer-sized counters). To keep the implementation details private, this
    /// method allows allocation of properly sized regions without exposing the exact type that
    /// will be stored on the heap.
    ///
    /// ```
    /// use static_alloc::{Slab, rc::Rc};
    ///
    /// struct Foo(u32);
    ///
    /// let slab: Slab<[u8; 1024]> = Slab::uninit();
    /// let layout = Rc::<Foo>::layout();
    /// assert!(layout.size() >= 4);
    ///
    /// let memory = slab.get_layout(layout).unwrap();
    ///
    /// // later:
    ///
    /// let rc = Rc::new(Foo(0), memory.uninit);
    /// ```
    pub fn layout() -> Layout {
        // FIXME: this should really be `const` but `Layout` does not offer that yet.
        Layout::new::<RcBox<T>>()
    }

    pub fn weak_count(rc: &Self) -> usize {
        rc.inner().weak.get()
    }

    pub fn strong_count(rc: &Self) -> usize {
        rc.inner().strong.get()
    }

    pub fn get_mut(rc: &mut Self) -> Option<&mut T> {
        unimplemented!()
    }

    /// Check if two `Rc`s point to the same data.
    ///
    /// This will never compare the values but simply inspect the inner pointers.
    ///
    /// ## Example
    ///
    /// ```
    /// use static_alloc::{Slab, rc::Rc};
    ///
    /// struct Foo;
    ///
    /// let slab: Slab<[u8; 1024]> = Slab::uninit();
    ///
    /// // Two Rc's pointing to the same data.
    /// let foo = slab.rc(Foo).unwrap();
    /// let foo2 = Rc::clone(&foo);
    ///
    /// // An unrelated allocation.
    /// let not_foo = slab.rc(Foo).unwrap();
    ///
    /// assert!( Rc::ptr_eq(&foo, &foo2));
    /// assert!(!Rc::ptr_eq(&foo, &not_foo));
    /// ```
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        this.inner.as_ptr() == other.inner.as_ptr()
    }

    /// Get a reference to the inner box.
    ///
    /// Note that we must not mutably touch or reference the inner `T` through the reference by
    /// casting to mutable pointers.
    fn inner(&self) -> &RcBox<T> {
        unsafe {
            self.inner.as_ref()
        }
    }

    fn as_mut_ptr(&mut self) -> *mut T {
        // `T` is the first member, #[repr(C)] makes this cast well behaved.
        self.inner.as_ptr() as *mut T
    }

    fn inc_strong(&self) {
        let val = Self::strong_count(self) + 1;
        self.inner().strong.set(val);
    }

    fn dec_strong(&self) {
        let val = Self::strong_count(self) - 1;
        self.inner().strong.set(val);
    }

    fn inc_weak(&self) {
        let val = Self::weak_count(self) + 1;
        self.inner().weak.set(val);
    }

    fn dec_weak(&self) {
        let val = Self::weak_count(self) - 1;
        self.inner().weak.set(val);
    }
}

impl<T> Drop for Rc<'_, T> {
    /// Drops the `Rc`.
    ///
    /// This will decrement the strong reference count. If the strong reference
    /// count reaches zero then the only other references (if any) are
    /// [`Weak`], so we `drop` the inner value.
    ///
    /// # Examples
    ///
    /// ```
    /// use static_alloc::{Slab, rc::Rc};
    ///
    /// struct Foo;
    ///
    /// impl Drop for Foo {
    ///     fn drop(&mut self) {
    ///         println!("dropped!");
    ///     }
    /// }
    ///
    /// let slab: Slab<[u8; 1024]> = Slab::uninit();
    ///
    /// let foo  = slab.rc(Foo).unwrap();
    /// let foo2 = Rc::clone(&foo);
    ///
    /// drop(foo);    // Doesn't print anything
    /// drop(foo2);   // Prints "dropped!"
    /// ```
    fn drop(&mut self) {
        self.dec_strong();
        // weak count doesn't actually do anything.
        if Rc::strong_count(self) == 0 {
            self.dec_weak();

            unsafe {
                ptr::drop_in_place(self.as_mut_ptr())
            }
        }
    }
}

impl<T> Clone for Rc<'_, T> {
    fn clone(&self) -> Self {
        self.inc_strong();
        Rc {
            inner: self.inner,
        }
    }
}

#[cfg(test)]
mod tests {
    use core::alloc::Layout;
    use core::cell::Cell;

    use super::{RcBox, Rc};
    use crate::Slab;

    #[test]
    fn layout_box_compatible() {
        let mut boxed = RcBox {
            val: 0usize,
            strong: Cell::new(1),
            weak: Cell::new(1),
        };

        let val_ptr = &boxed as *const _ as *const usize;
        assert_eq!(unsafe { *val_ptr }, 0);

        boxed.val = 0xdeadbeef;
        assert_eq!(unsafe { *val_ptr }, 0xdeadbeef);
    }

    #[test]
    fn control_through_counters() {
        struct Duck;
        struct NeverDrop;

        impl Drop for NeverDrop {
            fn drop(&mut self) {
                panic!("dropped!");
            }
        }

        let slab: Slab<[u8; 1024]> = Slab::uninit();
        let rc = slab.rc(NeverDrop).unwrap();
        rc.inc_strong();
        drop(rc);

        let rc = slab.rc(Duck).unwrap();
        // Forbidden in public, but we do not grab mutable references.
        let inner = rc.inner;
        drop(rc);

        unsafe {
            assert_eq!((*inner.as_ptr()).strong.get(), 0);
            assert_eq!((*inner.as_ptr()).weak.get(), 0);
        }
    }

    #[test]
    #[should_panic = "inner layout"]
    fn wrong_layout_panics() {
        struct Foo(u32);

        let slab: Slab<[u8; 1024]> = Slab::uninit();
        let wrong_alloc = slab.get_layout(Layout::new::<Foo>()).unwrap();

        let _ = Rc::new(Foo(0), wrong_alloc.uninit);
    }
}
