//! Heap buffers with a fixed alignment.
//!
//! Unbuffered I/O (`O_DIRECT` on Linux, `FILE_FLAG_NO_BUFFERING` on Windows)
//! needs the memory address, the file offset and the length to be multiples
//! of the device's logical sector size. [`AlignedBuffer`] always aligns to
//! [`ALIGNMENT`] (4 KiB), which is a multiple of every common sector size.

use std::alloc::{self, Layout};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// Alignment, in bytes, of every [`AlignedBuffer`].
pub const ALIGNMENT: usize = 4096;

/// A zero-initialised, 4 KiB-aligned byte buffer.
pub struct AlignedBuffer {
    ptr: NonNull<u8>,
    layout: Layout,
}

// SAFETY: the buffer owns its allocation exclusively; it is plain bytes.
unsafe impl Send for AlignedBuffer {}
// SAFETY: shared access only hands out `&[u8]`.
unsafe impl Sync for AlignedBuffer {}

impl AlignedBuffer {
    /// Allocates `len` zeroed bytes aligned to [`ALIGNMENT`].
    ///
    /// # Panics
    ///
    /// Panics if `len` is zero or not a multiple of [`ALIGNMENT`], and
    /// aborts if the allocation fails.
    pub fn new(len: usize) -> Self {
        assert!(len > 0 && len % ALIGNMENT == 0, "buffer length must be a non-zero multiple of {ALIGNMENT}");
        let layout = Layout::from_size_align(len, ALIGNMENT).expect("valid layout");
        // SAFETY: `layout` has a non-zero size.
        let raw = unsafe { alloc::alloc_zeroed(layout) };
        let ptr = NonNull::new(raw).unwrap_or_else(|| alloc::handle_alloc_error(layout));
        Self { ptr, layout }
    }
}

impl Deref for AlignedBuffer {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: `ptr` is valid for `layout.size()` initialised bytes.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.layout.size()) }
    }
}

impl DerefMut for AlignedBuffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` guarantees exclusive access.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.layout.size()) }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with exactly this layout.
        unsafe { alloc::dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

impl std::fmt::Debug for AlignedBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlignedBuffer").field("len", &self.layout.size()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_aligned_and_zeroed() {
        let buf = AlignedBuffer::new(3 * ALIGNMENT);
        assert_eq!(buf.as_ptr() as usize % ALIGNMENT, 0);
        assert_eq!(buf.len(), 3 * ALIGNMENT);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    #[should_panic]
    fn rejects_unaligned_length() {
        let _ = AlignedBuffer::new(100);
    }
}
