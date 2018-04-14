use memmap::{Mmap, MmapOptions};
use std::fs::File;
use std::io;

/// Return a read-only mmap view of the entire file, and its length.
///
/// For an empty file, return (1-byte mmap, 0) instead.
///
/// The caller might want to use some kind of locking to make
/// sure the file length is at some kind of boundary.
pub fn mmap_readonly(file: &File) -> io::Result<(Mmap, u64)> {
    let len = file.metadata()?.len();
    let mmap = unsafe {
        if len == 0 {
            MmapOptions::new().len(1).map_anon()?.make_read_only()?
        } else {
            MmapOptions::new().len(len as usize).map(&file)?
        }
    };
    Ok((mmap, len))
}
