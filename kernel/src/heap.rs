//! The kernel heap, so the network stack and the browser can use `Vec`,
//! `String` and `Box`. It is a fixed block of memory in .bss.

use core::ptr::addr_of_mut;

use linked_list_allocator::LockedHeap;

const HEAP_SIZE: usize = 48 * 1024 * 1024;

#[repr(align(4096))]
struct HeapMemory([u8; HEAP_SIZE]);

static mut HEAP_MEMORY: HeapMemory = HeapMemory([0; HEAP_SIZE]);

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub fn init() {
    unsafe {
        let start = addr_of_mut!(HEAP_MEMORY.0) as *mut u8;
        ALLOCATOR.lock().init(start, HEAP_SIZE);
    }
}
