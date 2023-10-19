use num_traits::AsPrimitive;
use std::mem::size_of;
use std::ops::{Index, IndexMut, Range};
use std::ptr::{slice_from_raw_parts, slice_from_raw_parts_mut};
use std::slice::{from_raw_parts, from_raw_parts_mut};

#[derive(Debug, Copy)]
pub struct Span<T> {
    ptr: *mut T,
    pub length: isize,
}
unsafe impl<T> Send for Span<T> {}
unsafe impl<T> Sync for Span<T> {}

impl<T> Span<T> {
    pub fn new<N: AsPrimitive<usize>>(ptr: *mut T, length: N) -> Span<T> {
        fn inner<T>(ptr: *mut T, length: usize) -> Span<T> {
            Span {
                ptr,
                length: length as isize,
            }
        }
        inner::<T>(ptr, length.as_())
    }

    pub fn ptr(&self) -> *mut T {
        self.ptr
    }

    pub fn len(&self) -> isize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub fn slice<N: AsPrimitive<usize>>(&self, index: N) -> Self {
        debug_assert!(index.as_() < self.length as usize);
        fn inner<T>(ptr: *mut T, index: usize, length: isize) -> Span<T> {
            unsafe {
                Span {
                    ptr: ptr.add(index),
                    length: length - index as isize,
                }
            }
        }
        inner::<T>(self.ptr, index.as_(), self.length)
    }

    pub fn range<N: AsPrimitive<usize>>(&self, index: usize, length: N) -> Self {
        debug_assert!(index < self.length as usize);
        debug_assert!(index + length.as_() <= self.length as usize);
        fn inner<T>(ptr: *mut T, index: usize, length: usize) -> Span<T> {
            unsafe {
                Span {
                    ptr: ptr.add(index),
                    length: length as isize,
                }
            }
        }
        inner::<T>(self.ptr, index, length.as_())
    }

    pub fn slice_size<N: AsPrimitive<usize>>(&self, length: N) -> Self {
        debug_assert!(length.as_() <= self.length as usize);
        fn inner<T>(ptr: *mut T, length: usize) -> Span<T> {
            Span {
                ptr,
                length: length as isize,
            }
        }
        inner::<T>(self.ptr, length.as_())
    }

    pub fn cast<U>(&self) -> Span<U> {
        let t_size = size_of::<T>();
        let u_size = size_of::<U>();
        if t_size >= u_size {
            let size_modifier = t_size / size_of::<U>();
            unsafe {
                Span::<U>::new(
                    self.ptr.offset(0).cast(),
                    self.length as usize * size_modifier,
                )
            }
        } else {
            let size_modifier = u_size / size_of::<T>();
            unsafe {
                Span::<U>::new(
                    self.ptr.offset(0).cast(),
                    self.length as usize / size_modifier,
                )
            }
        }
    }
}

impl<T> AsRef<[T]> for Span<T> {
    fn as_ref(&self) -> &[T] {
        unsafe { from_raw_parts(self.ptr, self.length as usize) }
    }
}

impl<T> AsMut<[T]> for Span<T> {
    fn as_mut(&mut self) -> &mut [T] {
        unsafe { from_raw_parts_mut(self.ptr, self.length as usize) }
    }
}

impl<T> Clone for Span<T> {
    fn clone(&self) -> Self {
        unsafe { Self::new(self.ptr.offset(0), self.length) }
    }
}
impl<T, N: AsPrimitive<isize>> Index<Range<N>> for Span<T> {
    type Output = [T];
    fn index(&self, index: Range<N>) -> &Self::Output {
        let start = index.start.as_();
        let end = index.end.as_();
        debug_assert!(start >= 0);
        debug_assert!(start < end);
        debug_assert!(start < self.length);
        debug_assert!(end <= self.length);
        unsafe { &*slice_from_raw_parts(self.ptr.offset(start), (end - start) as usize) }
    }
}
impl<T, N: AsPrimitive<isize>> IndexMut<Range<N>> for Span<T> {
    fn index_mut(&mut self, index: Range<N>) -> &mut Self::Output {
        let start = index.start.as_();
        let end = index.end.as_();
        debug_assert!(start >= 0);
        debug_assert!(start < end);
        debug_assert!(start < self.length);
        debug_assert!(end <= self.length);
        unsafe { &mut *slice_from_raw_parts_mut(self.ptr.offset(start), (end - start) as usize) }
    }
}

macro_rules! impl_span_index {
    ($($name: ident);*) => {
        $(
            impl<T> Index<$name> for Span<T> {
                type Output = T;
                fn index(&self, index: $name) -> &Self::Output {
                    let index = index as isize;
                    debug_assert!(index < self.length);
                    debug_assert!(index >= 0);
                    unsafe {
                        &*self.ptr.offset(index)
                    }
                }
            }
            impl<T> IndexMut<$name> for Span<T> {
                fn index_mut(&mut self, index: $name) -> &mut Self::Output {
                    let index = index as isize;
                    debug_assert!(index < self.length);
                    debug_assert!(index >= 0);
                    unsafe {
                        &mut *self.ptr.offset(index)
                    }
                }
            }
        )*
    };
    ()=>{};
}
impl_span_index!(
    usize;
    u8;
    u16;
    u32;
    u64;
    isize;
    i8;
    i16;
    i32;
    i64
);
