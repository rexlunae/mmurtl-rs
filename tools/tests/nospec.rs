#[path = "../../src/arch/amd64/nospec.rs"]
mod nospec;
mod arch { pub(crate) use super::nospec; }
#[path = "../../src/memory/user_bounds.rs"]
mod user_bounds;
fn main() {
    use nospec::{array_index_mask_nospec as mask, array_index_nospec as index};
    for size in [0, 1, 11, 65, usize::MAX] {
        for i in [0, 1, 10, 11, 64, 65, usize::MAX - 1, usize::MAX] {
            assert_eq!(mask(i, size), if i < size { usize::MAX } else { 0 });
            assert_eq!(index(i, size), if i < size { i } else { 0 });
        }
    }
    const BASE: u64 = 0x640000000000;
    const END: u64 = BASE + 64 * 1024 * 1024;
    let pointers = [0, BASE - 1, BASE, BASE + 1, END - 1, END, END + 1, u64::MAX];
    let lengths = [0, 1, 2, END - BASE, END - BASE + 1, u64::MAX];
    for ptr in pointers {
        for len in lengths {
            let valid = ptr >= BASE && ptr < END && ptr.checked_add(len).is_some_and(|x| x <= END);
            assert_eq!(user_bounds::mask_user_ptr(ptr, len, BASE, END), if valid { ptr } else { END });
            assert_eq!(user_bounds::bounds_ok(ptr, len, BASE, END), len == 0 || valid);
        }
    }
    nospec::barrier_nospec();
    println!("nospec: unsigned extremes, empty bounds, syscall indices and pointer overflow passed");
}
