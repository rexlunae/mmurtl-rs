//! Shared filesystem interface for the current virtio block device.
//! FAT12/16/32 (including VFAT long names) and exFAT share shell/ELF APIs.

pub mod exfat;
pub mod fat;

use alloc::{string::String, vec::Vec};

/// Discover FAT volumes first, otherwise run the existing exFAT boot demo.
pub fn init() {
    if !fat::mount() {
        exfat::demo();
    }
}

pub fn read_dir(path: &str) -> Option<Vec<(String, bool, u64)>> {
    if fat::mounted() {
        fat::read_dir(path)
    } else {
        exfat::read_dir(path)
    }
}
pub fn read_file(path: &str) -> Option<Vec<u8>> {
    if fat::mounted() {
        fat::read_file(path)
    } else {
        exfat::read_file(path)
    }
}
pub fn metadata(path: &str) -> Option<(bool, u64)> {
    if fat::mounted() {
        fat::metadata(path)
    } else {
        exfat::metadata(path)
    }
}
pub fn directory_id(path: &str) -> Option<u64> {
    if fat::mounted() {
        fat::directory_id(path)
    } else {
        exfat::directory_id(path)
    }
}
pub fn create_file(path: &str, data: &[u8]) -> Result<(), &'static str> {
    if fat::mounted() {
        fat::create_file(path, data)
    } else {
        exfat::create_file(path, data)
    }
}
pub fn write_file(path: &str, data: &[u8]) -> Result<(), &'static str> {
    if fat::mounted() {
        fat::write_file(path, data)
    } else {
        exfat::write_file(path, data)
    }
}
pub fn append_file(path: &str, data: &[u8]) -> Result<(), &'static str> {
    if fat::mounted() {
        fat::append_file(path, data)
    } else {
        exfat::append_file(path, data)
    }
}
pub fn delete(path: &str) -> Result<(), &'static str> {
    if fat::mounted() {
        fat::delete(path)
    } else {
        exfat::delete(path)
    }
}
pub fn mkdir(path: &str) -> Result<(), &'static str> {
    if fat::mounted() {
        fat::mkdir(path)
    } else {
        exfat::mkdir(path)
    }
}
