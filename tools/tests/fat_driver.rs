//! Host integration harness: run the kernel FAT driver against an image.
//! Compile with rustc --edition=2021 tools/tests/fat_driver.rs -o ...
extern crate alloc;
extern crate self as spin;

pub struct Mutex<T>(std::sync::Mutex<T>);
impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self(std::sync::Mutex::new(value))
    }
    pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.0.lock().unwrap()
    }
}
pub mod serial {
    pub fn write_str(s: &str) {
        print!("{s}");
    }
    pub fn write_line(s: &str) {
        println!("{s}");
    }
}
pub mod virtio {
    pub mod blk {
        use crate::Mutex;
        use std::sync::atomic::{AtomicIsize, Ordering};
        pub static FAIL_AFTER: AtomicIsize = AtomicIsize::new(-1);
        pub static DISK: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        pub fn capacity_sectors() -> u64 {
            DISK.lock().len() as u64 / 512
        }
        pub fn read_sectors(lba: u64, buffer: &mut [u8]) -> bool {
            let disk = DISK.lock();
            let start = lba as usize * 512;
            match disk.get(start..start + buffer.len()) {
                Some(bytes) => {
                    buffer.copy_from_slice(bytes);
                    true
                }
                None => false,
            }
        }
        pub fn write_sectors(lba: u64, buffer: &mut [u8]) -> bool {
            let left = FAIL_AFTER.load(Ordering::Relaxed);
            if left == 0 {
                return false;
            }
            if left > 0 {
                FAIL_AFTER.fetch_sub(1, Ordering::Relaxed);
            }
            let mut disk = DISK.lock();
            let start = lba as usize * 512;
            match disk.get_mut(start..start + buffer.len()) {
                Some(bytes) => {
                    bytes.copy_from_slice(buffer);
                    true
                }
                None => false,
            }
        }
    }
}
#[path = "../../src/fs/fat.rs"]
mod fat;

fn main() {
    let path = std::env::args().nth(1).expect("image path");
    *virtio::blk::DISK.lock() = std::fs::read(&path).unwrap();
    let mode = std::env::args().nth(2);
    if mode.as_deref() == Some("--reject") {
        assert!(!fat::mount());
        return;
    }
    assert!(fat::mount());
    assert!(fat::mounted());
    if mode.as_deref() == Some("--inspect-efi") {
        let files = fat::read_dir("EFI/BOOT").expect("EFI boot directory");
        assert!(files
            .iter()
            .any(|(name, dir, _)| !dir && name.eq_ignore_ascii_case("BOOTX64.EFI")));
        println!("Generated UEFI image: EFI/BOOT/BOOTX64.EFI found");
        return;
    }
    if mode.as_deref() == Some("--failure") {
        use std::sync::atomic::Ordering;
        fat::write_file("Overwrite long filename.txt", b"original").unwrap();
        let baseline = virtio::blk::DISK.lock().clone();
        let replacement = vec![b'N'; 3000];
        let mut errors = 0;
        for cut in 0..25 {
            *virtio::blk::DISK.lock() = baseline.clone();
            assert!(fat::mount());
            virtio::blk::FAIL_AFTER.store(cut, Ordering::Relaxed);
            if fat::write_file("Overwrite long filename.txt", &replacement).is_err() {
                errors += 1;
                assert!(fat::read_file("Overwrite long filename.txt").is_none());
            }
            virtio::blk::FAIL_AFTER.store(-1, Ordering::Relaxed);
            assert!(fat::mount());
            let data = fat::read_file("Overwrite long filename.txt")
                .expect("live file must remain readable");
            assert!(
                data == b"original" || data == replacement,
                "never publish incomplete data or free a live allocation"
            );
        }
        assert!(errors > 0 && errors < 25);
        println!("Injected write failures: original or complete replacement preserved");
        return;
    }
    if mode.as_deref() == Some("--root-full") {
        let mut names = Vec::new();
        for index in 0..100 {
            let name = format!("{index:03}{}", "x".repeat(252));
            match fat::write_file(&name, b"contents") {
                Ok(()) => names.push(name),
                Err("FAT12/16 root directory full") => break,
                Err(e) => panic!("unexpected root error: {e}"),
            }
        }
        assert!(!names.is_empty() && names.len() < 100);
        assert_eq!(fat::read_dir("/").unwrap().len(), names.len());
        for name in names {
            fat::delete(&name).unwrap();
        }
        std::fs::write(&path, &*virtio::blk::DISK.lock()).unwrap();
        println!("Fixed root directory full: failed allocation rolled back");
        return;
    }
    if std::env::args().nth(2).as_deref() == Some("--seed") {
        let program = std::env::args().nth(3).expect("ELF path");
        fat::mkdir("BIN").unwrap();
        fat::create_file("BIN/HELLO.ELF", &std::fs::read(program).unwrap()).unwrap();
        std::fs::write(&path, &*virtio::blk::DISK.lock()).unwrap();
        return;
    }
    fat::mkdir("A long directory name").unwrap();
    assert!(fat::directory_id("A long directory name").is_some());
    assert_eq!(fat::directory_id("A long directory name"), fat::directory_id("ALONGD~1"));
    let file = "A long directory name/A long filename with spaces.txt";
    fat::create_file(file, b"original").unwrap();
    assert!(fat::create_file(file, b"clobber").is_err());
    assert_eq!(fat::read_file(file).unwrap(), b"original");
    assert_eq!(fat::metadata(file), Some((false, 8)));
    assert_eq!(
        fat::read_file("a long directory name/a long filename WITH spaces.TXT").unwrap(),
        b"original"
    );
    assert_eq!(
        fat::read_file("ALONGD~1/ALONGF~1.TXT").unwrap(),
        b"original"
    );
    fat::append_file(file, b" appended").unwrap();
    assert_eq!(fat::read_file(file).unwrap(), b"original appended");
    let large: Vec<u8> = (0..40_000).map(|i| (i % 251) as u8).collect();
    fat::write_file(file, &large).unwrap();
    assert_eq!(fat::read_file(file).unwrap(), large);
    fat::write_file(file, b"small").unwrap();
    assert_eq!(fat::read_file(file).unwrap(), b"small");
    fat::write_file("A long directory name/empty", b"").unwrap();
    assert_eq!(fat::read_file("A long directory name/empty").unwrap(), b"");
    fat::write_file("A long directory name/café 😀.txt", b"unicode").unwrap();
    assert_eq!(
        fat::read_file("A long directory name/CAFÉ 😀.TXT").unwrap(),
        b"unicode"
    );
    let max_name = "x".repeat(255);
    fat::write_file(&max_name, b"255 UTF-16 units").unwrap();
    assert_eq!(fat::read_file(&max_name).unwrap(), b"255 UTF-16 units");
    assert!(fat::write_file(&"x".repeat(256), b"invalid").is_err());
    assert!(fat::write_file("invalid?.txt", b"invalid").is_err());
    // Alias collisions and directory growth across multiple clusters.
    for i in 0..40 {
        fat::write_file(
            &format!("A long directory name/Collision filename number {i:03}.txt"),
            &[i],
        )
        .unwrap();
    }
    let names = fat::read_dir("A long directory name").unwrap();
    assert_eq!(names.len(), 43);
    assert!(names.iter().any(|(name, _, _)| name == "café 😀.txt"));
    for i in 0..40 {
        assert_eq!(
            fat::read_file(&format!(
                "A long directory name/Collision filename number {i:03}.txt"
            ))
            .unwrap(),
            [i]
        );
        fat::delete(&format!(
            "A long directory name/Collision filename number {i:03}.txt"
        ))
        .unwrap();
    }
    assert!(fat::delete("A long directory name").is_err());
    fat::delete(file).unwrap();
    assert!(fat::read_file(file).is_none());
    fat::delete("A long directory name/empty").unwrap();
    fat::delete("A long directory name/café 😀.txt").unwrap();
    fat::delete("A long directory name").unwrap();
    fat::delete(&max_name).unwrap();
    // Exercise FAT12 entries spanning a sector and fragmented allocation.
    fat::mkdir("fragmented").unwrap();
    for i in 0..340 {
        fat::write_file(&format!("fragmented/F{i:03}.TXT"), b"one cluster").unwrap();
    }
    for i in (0..340).step_by(2) {
        fat::delete(&format!("fragmented/F{i:03}.TXT")).unwrap();
    }
    fat::write_file("fragmented/fragmented data.bin", &large).unwrap();
    assert_eq!(
        fat::read_file("fragmented/fragmented data.bin").unwrap(),
        large
    );
    fat::delete("fragmented/fragmented data.bin").unwrap();
    for i in (1..340).step_by(2) {
        fat::delete(&format!("fragmented/F{i:03}.TXT")).unwrap();
    }
    fat::delete("fragmented").unwrap();
    // Leave interoperable long names for fsck/host readers, then remount.
    fat::mkdir("Long directory").unwrap();
    fat::write_file(
        "Long directory/Long filename café 😀.txt",
        b"host verification",
    )
    .unwrap();
    assert!(fat::mount());
    assert_eq!(
        fat::read_file("Long directory/Long filename café 😀.txt").unwrap(),
        b"host verification"
    );
    std::fs::write(&path, &*virtio::blk::DISK.lock()).unwrap();
    println!("FAT driver integration passed");
}
