#![no_std]

use core::alloc::Layout;
use core::ptr::NonNull;

use allocator::{AllocError, AllocResult, BaseAllocator, ByteAllocator, PageAllocator};

/// Early memory allocator
/// Use it before formal bytes-allocator and pages-allocator can work!
/// This is a double-end memory range:
/// - Alloc bytes forward
/// - Alloc pages backward
///
/// [ bytes-used | avail-area | pages-used ]
/// |            | -->    <-- |            |
/// start       b_pos        p_pos       end
///
/// For bytes area, 'count' records number of allocations.
/// When it goes down to ZERO, free bytes-used area.
/// For pages area, it will never be freed!
pub struct EarlyAllocator<const SIZE: usize> {
    start: usize,
    end: usize,
    byte_pos: usize,
    page_pos: usize,
    bytes_count: usize,
    used_pages: usize,
}

impl<const SIZE: usize> EarlyAllocator<SIZE> {
    /// Creates an empty [`EarlyAllocator`].
    pub const fn new() -> Self {
        Self {
            start: 0,
            end: 0,
            byte_pos: 0,
            page_pos: 0,
            bytes_count: 0,
            used_pages: 0,
        }
    }
}

impl<const SIZE: usize> Default for EarlyAllocator<SIZE> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SIZE: usize> BaseAllocator for EarlyAllocator<SIZE> {
    fn init(&mut self, start: usize, size: usize) {
        assert!(SIZE.is_power_of_two());
        self.start = start;
        self.end = start + size;
        self.byte_pos = start;
        self.page_pos = align_down(self.end, SIZE).max(start);
        self.bytes_count = 0;
        self.used_pages = 0;
    }

    fn add_memory(&mut self, _start: usize, _size: usize) -> AllocResult {
        Err(AllocError::NoMemory)
    }
}

impl<const SIZE: usize> ByteAllocator for EarlyAllocator<SIZE> {
    fn alloc(&mut self, layout: Layout) -> AllocResult<NonNull<u8>> {
        let alloc_start = align_up(self.byte_pos, layout.align()).ok_or(AllocError::NoMemory)?;
        let alloc_end = alloc_start
            .checked_add(layout.size())
            .ok_or(AllocError::NoMemory)?;
        if alloc_end > self.page_pos {
            return Err(AllocError::NoMemory);
        }
        let ptr = NonNull::new(alloc_start as *mut u8).ok_or(AllocError::NoMemory)?;
        self.byte_pos = alloc_end;
        self.bytes_count += 1;
        Ok(ptr)
    }

    fn dealloc(&mut self, _pos: NonNull<u8>, _layout: Layout) {
        self.bytes_count = self.bytes_count.saturating_sub(1);
        if self.bytes_count == 0 {
            self.byte_pos = self.start;
        }
    }

    fn total_bytes(&self) -> usize {
        self.end - self.start
    }

    fn used_bytes(&self) -> usize {
        self.byte_pos - self.start
    }

    fn available_bytes(&self) -> usize {
        self.page_pos.saturating_sub(self.byte_pos)
    }
}

impl<const SIZE: usize> PageAllocator for EarlyAllocator<SIZE> {
    const PAGE_SIZE: usize = SIZE;

    fn alloc_pages(&mut self, num_pages: usize, align_pow2: usize) -> AllocResult<usize> {
        if num_pages == 0 || align_pow2 % SIZE != 0 || !(align_pow2 / SIZE).is_power_of_two() {
            return Err(AllocError::InvalidParam);
        }
        let alloc_size = num_pages.checked_mul(SIZE).ok_or(AllocError::NoMemory)?;
        let new_pos = self
            .page_pos
            .checked_sub(alloc_size)
            .ok_or(AllocError::NoMemory)?;
        let aligned_pos = align_down(new_pos, align_pow2);
        if aligned_pos < self.byte_pos {
            return Err(AllocError::NoMemory);
        }
        self.page_pos = aligned_pos;
        self.used_pages += num_pages;
        Ok(aligned_pos)
    }

    fn dealloc_pages(&mut self, _pos: usize, _num_pages: usize) {}

    fn total_pages(&self) -> usize {
        let page_start = align_up(self.start, SIZE).unwrap_or(self.end);
        let page_end = align_down(self.end, SIZE);
        page_end.saturating_sub(page_start) / SIZE
    }

    fn used_pages(&self) -> usize {
        self.used_pages
    }

    fn available_pages(&self) -> usize {
        let byte_end = align_up(self.byte_pos, SIZE).unwrap_or(self.page_pos);
        self.page_pos.saturating_sub(byte_end) / SIZE
    }
}

#[inline]
const fn align_down(pos: usize, align: usize) -> usize {
    pos & !(align - 1)
}

#[inline]
fn align_up(pos: usize, align: usize) -> Option<usize> {
    pos.checked_add(align - 1).map(|p| p & !(align - 1))
}
