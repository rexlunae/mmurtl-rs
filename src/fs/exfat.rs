//! exFAT filesystem driver (read + create) on virtio-blk.
//!
//! Implements the on-disk format from the Microsoft exFAT specification:
//!   - Boot sector: geometry, FAT offset, cluster heap, root directory
//!   - Directory entry sets: File (0x85) + Stream Extension (0xC0) +
//!     FileName (0xC1), with the rotate-right entry-set checksum
//!   - Cluster chains via the FAT, plus the NoFatChain contiguous fast path
//!   - Allocation Bitmap (0x81) for cluster accounting
//!
//! Supported: mount, list root directory, read files (chained or
//! contiguous), and create files in the root directory. Created files use
//! contiguous NoFatChain allocation (spec-conformant: the FAT is not
//! consulted for such files), so only the data clusters, the allocation
//! bitmap, and the directory need to be written.
//!
//! Limitations (documented, not surprising): root-directory files only,
//! ASCII names up to 30 chars, no delete/rename/append, directory must
//! have free entry slots in its existing clusters, 512-byte sectors.

use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use crate::virtio::blk;

const SECTOR: usize = 512;
const DIR_ENTRY: usize = 32;

// Directory entry types
const ET_END: u8 = 0x00;
const ET_BITMAP: u8 = 0x81;
const ET_LABEL: u8 = 0x83;
const ET_FILE: u8 = 0x85;
const ET_STREAM: u8 = 0xC0;
const ET_NAME: u8 = 0xC1;

// Stream extension GeneralSecondaryFlags
const FLAG_ALLOC_POSSIBLE: u8 = 0x01;
const FLAG_NO_FAT_CHAIN: u8 = 0x02;

/// FAT end-of-chain marker
const FAT_EOF: u32 = 0xFFFF_FFFF;

// ========================================================================
// Volume state
// ========================================================================

struct Volume {
    sectors_per_cluster: u32,
    /// LBA of the first FAT
    fat_lba: u64,
    /// LBA of the cluster heap (cluster 2 starts here)
    heap_lba: u64,
    cluster_count: u32,
    root_cluster: u32,
    /// Allocation bitmap location (from the 0x81 root entry)
    bitmap_cluster: u32,
    bitmap_bytes: u64,
}

impl Volume {
    fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR
    }
    fn cluster_lba(&self, cluster: u32) -> u64 {
        self.heap_lba + (cluster as u64 - 2) * self.sectors_per_cluster as u64
    }
}

static VOLUME: Mutex<Option<Volume>> = Mutex::new(None);

// ========================================================================
// Low-level helpers
// ========================================================================

fn read_sector(lba: u64, buf: &mut [u8; SECTOR]) -> bool {
    blk::read_sectors(lba, buf)
}

fn write_sector(lba: u64, buf: &mut [u8; SECTOR]) -> bool {
    blk::write_sectors(lba, buf)
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// Read one FAT entry
fn fat_entry(vol: &Volume, cluster: u32) -> u32 {
    let byte_off = cluster as u64 * 4;
    let mut buf = [0u8; SECTOR];
    read_sector(vol.fat_lba + byte_off / SECTOR as u64, &mut buf);
    le32(&buf[(byte_off % SECTOR as u64) as usize..])
}

/// Read an entire cluster into `out` (must be cluster_bytes long)
fn read_cluster(vol: &Volume, cluster: u32, out: &mut [u8]) -> bool {
    let lba = vol.cluster_lba(cluster);
    for s in 0..vol.sectors_per_cluster as u64 {
        let mut buf = [0u8; SECTOR];
        if !read_sector(lba + s, &mut buf) {
            return false;
        }
        let off = s as usize * SECTOR;
        out[off..off + SECTOR].copy_from_slice(&buf);
    }
    true
}

/// The rotate-right checksum used for both entry sets (u16) — bytes 2-3
/// of the first entry (the checksum field itself) are skipped.
fn entry_set_checksum(set: &[u8]) -> u16 {
    let mut sum: u16 = 0;
    for (i, b) in set.iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        sum = sum.rotate_right(1).wrapping_add(*b as u16);
    }
    sum
}

/// Name hash over the up-cased UTF-16LE file name
fn name_hash(name_utf16: &[u16]) -> u16 {
    let mut sum: u16 = 0;
    for ch in name_utf16 {
        for b in ch.to_le_bytes() {
            sum = sum.rotate_right(1).wrapping_add(b as u16);
        }
    }
    sum
}

// ========================================================================
// Directory scanning
// ========================================================================

/// A parsed file entry set from a directory
pub struct DirFile {
    pub name: heapless::String<64>,
    pub attributes: u16,
    pub first_cluster: u32,
    pub data_length: u64,
    pub no_fat_chain: bool,
}

/// Walk a directory's clusters (FAT-chained), invoking `f` for every
/// complete file entry set. Returns early if `f` returns false.
fn scan_dir(vol: &Volume, first_cluster: u32, mut f: impl FnMut(&DirFile) -> bool) {
    let cbytes = vol.cluster_bytes();
    let mut cluster_buf = vec![0u8; cbytes];

    // In-progress entry set state
    let mut current: Option<DirFile> = None;
    let mut names_left = 0u8;

    let mut cluster = first_cluster;
    loop {
        if cluster < 2 || !read_cluster(vol, cluster, &mut cluster_buf) {
            return;
        }
        for e in cluster_buf.chunks_exact(DIR_ENTRY) {
            match e[0] {
                ET_END => return,
                ET_FILE => {
                    current = Some(DirFile {
                        name: heapless::String::new(),
                        attributes: le16(&e[4..]),
                        first_cluster: 0,
                        data_length: 0,
                        no_fat_chain: false,
                    });
                    names_left = 0;
                }
                ET_STREAM => {
                    if let Some(df) = current.as_mut() {
                        df.no_fat_chain = e[1] & FLAG_NO_FAT_CHAIN != 0;
                        df.first_cluster = le32(&e[20..]);
                        df.data_length = le64(&e[24..]);
                        // NameLength in chars → number of 0xC1 entries
                        names_left = (e[3] + 14) / 15;
                    }
                }
                ET_NAME => {
                    if let Some(df) = current.as_mut() {
                        for ch in e[2..32].chunks_exact(2) {
                            let c = le16(ch);
                            if c != 0 && df.name.len() < 63 {
                                let ascii = if c < 0x80 { c as u8 as char } else { '?' };
                                let _ = df.name.push(ascii);
                            }
                        }
                        if names_left > 0 {
                            names_left -= 1;
                            if names_left == 0 {
                                if let Some(df) = current.take() {
                                    if !f(&df) {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let next = fat_entry(vol, cluster);
        if next < 2 || next == FAT_EOF || next > vol.cluster_count + 1 {
            return;
        }
        cluster = next;
    }
}

fn names_equal_ascii(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .all(|(x, y)| x.to_ascii_uppercase() == y.to_ascii_uppercase())
}

// ========================================================================
// Mount
// ========================================================================

/// Mount the exFAT volume on the virtio-blk device. Returns false if
/// there is no device or no exFAT signature.
pub fn mount() -> bool {
    if !blk::available() {
        crate::serial::write_line("[FS] No block device — exFAT not mounted");
        return false;
    }

    let mut bs = [0u8; SECTOR];
    if !read_sector(0, &mut bs) || &bs[3..11] != b"EXFAT   " || bs[510] != 0x55 || bs[511] != 0xAA
    {
        crate::serial::write_line("[FS] No exFAT filesystem on block device");
        return false;
    }

    let bytes_per_sector_shift = bs[108];
    if bytes_per_sector_shift != 9 {
        crate::serial::write_line("[FS] Unsupported sector size (need 512)");
        return false;
    }

    let mut vol = Volume {
        sectors_per_cluster: 1 << bs[109],
        fat_lba: le32(&bs[80..]) as u64,
        heap_lba: le32(&bs[88..]) as u64,
        cluster_count: le32(&bs[92..]),
        root_cluster: le32(&bs[96..]),
        bitmap_cluster: 0,
        bitmap_bytes: 0,
    };

    // Find the allocation bitmap + volume label in the root directory
    let mut label: heapless::String<24> = heapless::String::new();
    {
        let cbytes = vol.cluster_bytes();
        let mut buf = vec![0u8; cbytes];
        let mut cluster = vol.root_cluster;
        'outer: loop {
            if cluster < 2 || !read_cluster(&vol, cluster, &mut buf) {
                break;
            }
            for e in buf.chunks_exact(DIR_ENTRY) {
                match e[0] {
                    ET_END => break 'outer,
                    ET_BITMAP => {
                        vol.bitmap_cluster = le32(&e[20..]);
                        vol.bitmap_bytes = le64(&e[24..]);
                    }
                    ET_LABEL => {
                        let count = (e[1] as usize).min(11);
                        for ch in e[2..2 + count * 2].chunks_exact(2) {
                            let c = le16(ch);
                            let _ = label.push(if c < 0x80 { c as u8 as char } else { '?' });
                        }
                    }
                    _ => {}
                }
            }
            let next = fat_entry(&vol, cluster);
            if next < 2 || next == FAT_EOF {
                break;
            }
            cluster = next;
        }
    }

    if vol.bitmap_cluster < 2 {
        crate::serial::write_line("[FS] Allocation bitmap not found — refusing to mount");
        return false;
    }

    crate::serial::write_str("[FS] exFAT mounted: label \"");
    crate::serial::write_str(&label);
    crate::serial::write_str("\", ");
    crate::serial::write_dec(vol.cluster_count as u64);
    crate::serial::write_str(" clusters x ");
    crate::serial::write_dec(vol.cluster_bytes() as u64 / 1024);
    crate::serial::write_str(" KiB, root @ cluster ");
    crate::serial::write_dec(vol.root_cluster as u64);
    crate::serial::write_str("\n");

    *VOLUME.lock() = Some(vol);
    true
}

pub fn mounted() -> bool {
    VOLUME.lock().is_some()
}

// ========================================================================
// Read paths
// ========================================================================

/// Print the root directory listing
pub fn list_root() {
    let guard = VOLUME.lock();
    let vol = match guard.as_ref() {
        Some(v) => v,
        None => return,
    };

    crate::serial::write_line("[FS] Root directory:");
    let mut count = 0u32;
    scan_dir(vol, vol.root_cluster, |f| {
        crate::serial::write_str("[FS]   ");
        crate::serial::write_str(&f.name);
        crate::serial::write_str("  (");
        crate::serial::write_dec(f.data_length);
        crate::serial::write_str(" bytes");
        if f.attributes & 0x10 != 0 {
            crate::serial::write_str(", dir");
        }
        crate::serial::write_str(")\n");
        count += 1;
        true
    });
    if count == 0 {
        crate::serial::write_line("[FS]   (empty)");
    }
}

/// Read a whole file from the root directory by name (ASCII,
/// case-insensitive). Returns None if absent or on I/O error.
pub fn read_file(name: &str) -> Option<Vec<u8>> {
    let guard = VOLUME.lock();
    let vol = guard.as_ref()?;

    let mut found: Option<(u32, u64, bool)> = None;
    scan_dir(vol, vol.root_cluster, |f| {
        if names_equal_ascii(&f.name, name) && f.attributes & 0x10 == 0 {
            found = Some((f.first_cluster, f.data_length, f.no_fat_chain));
            false
        } else {
            true
        }
    });
    let (first, len, no_fat_chain) = found?;
    if len == 0 {
        return Some(Vec::new());
    }
    if first < 2 {
        return None;
    }

    let cbytes = vol.cluster_bytes();
    let mut data = Vec::with_capacity(len as usize);
    let mut cluster_buf = vec![0u8; cbytes];
    let mut cluster = first;
    let mut remaining = len as usize;

    while remaining > 0 {
        if !read_cluster(vol, cluster, &mut cluster_buf) {
            return None;
        }
        let take = remaining.min(cbytes);
        data.extend_from_slice(&cluster_buf[..take]);
        remaining -= take;
        if remaining == 0 {
            break;
        }
        cluster = if no_fat_chain {
            cluster + 1
        } else {
            let next = fat_entry(vol, cluster);
            if next < 2 || next == FAT_EOF {
                return None; // chain shorter than data_length
            }
            next
        };
    }
    Some(data)
}

// ========================================================================
// Write path: create a file in the root directory
// ========================================================================

/// Fixed timestamp for created files: 2026-07-17 08:00:00 UTC
const TIMESTAMP: u32 = ((2026 - 1980) << 25) | (7 << 21) | (17 << 16) | (8 << 11);

/// Allocate `count` contiguous clusters from the allocation bitmap.
/// Returns the first cluster number. Bit N of the bitmap = cluster N+2.
fn bitmap_alloc(vol: &Volume, count: usize) -> Option<u32> {
    let bitmap_lba = vol.cluster_lba(vol.bitmap_cluster);
    let sectors = (vol.bitmap_bytes as usize + SECTOR - 1) / SECTOR;

    // Find a run of `count` clear bits (may span sectors)
    let mut run_start = 0usize;
    let mut run_len = 0usize;
    let mut found = None;
    let mut buf = [0u8; SECTOR];
    'search: for s in 0..sectors {
        if !read_sector(bitmap_lba + s as u64, &mut buf) {
            return None;
        }
        for bit_in_sector in 0..SECTOR * 8 {
            let bit = s * SECTOR * 8 + bit_in_sector;
            if bit >= vol.cluster_count as usize {
                break 'search;
            }
            if buf[bit_in_sector / 8] & (1 << (bit_in_sector % 8)) == 0 {
                if run_len == 0 {
                    run_start = bit;
                }
                run_len += 1;
                if run_len == count {
                    found = Some(run_start);
                    break 'search;
                }
            } else {
                run_len = 0;
            }
        }
    }
    let start_bit = found?;

    // Set the bits (read-modify-write each affected sector)
    let mut bit = start_bit;
    let end = start_bit + count;
    while bit < end {
        let sector = bit / (SECTOR * 8);
        if !read_sector(bitmap_lba + sector as u64, &mut buf) {
            return None;
        }
        while bit < end && bit / (SECTOR * 8) == sector {
            buf[(bit % (SECTOR * 8)) / 8] |= 1 << (bit % 8);
            bit += 1;
        }
        if !write_sector(bitmap_lba + sector as u64, &mut buf) {
            return None;
        }
    }

    Some(start_bit as u32 + 2)
}

/// Create `name` (ASCII, ≤ 30 chars) in the root directory with `data`.
/// Uses contiguous NoFatChain allocation. Fails if the file exists or the
/// root directory has no free entry slots in its existing clusters.
pub fn create_file(name: &str, data: &[u8]) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > 30 || !name.is_ascii() {
        return Err("name must be ASCII, 1-30 chars");
    }
    if read_file(name).is_some() {
        return Err("file already exists");
    }

    let guard = VOLUME.lock();
    let vol = guard.as_ref().ok_or("not mounted")?;
    let cbytes = vol.cluster_bytes();

    // 1. Allocate contiguous clusters and write the data
    let clusters = (data.len() + cbytes - 1) / cbytes;
    let first_cluster = if clusters > 0 {
        let first = bitmap_alloc(vol, clusters).ok_or("no contiguous space")?;
        let mut sec = [0u8; SECTOR];
        let mut written = 0usize;
        let mut lba = vol.cluster_lba(first);
        while written < data.len() {
            let take = (data.len() - written).min(SECTOR);
            sec[..take].copy_from_slice(&data[written..written + take]);
            sec[take..].fill(0);
            if !write_sector(lba, &mut sec) {
                return Err("data write failed");
            }
            written += take;
            lba += 1;
        }
        first
    } else {
        0
    };

    // 2. Build the directory entry set: File + Stream + FileName entries
    let name_utf16: Vec<u16> = name.bytes().map(|b| b as u16).collect();
    let name_entries = (name.len() + 14) / 15;
    let secondary_count = 1 + name_entries as u8;
    let set_len = (1 + 1 + name_entries) * DIR_ENTRY;
    let mut set = vec![0u8; set_len];

    // File directory entry (0x85)
    set[0] = ET_FILE;
    set[1] = secondary_count;
    set[4..6].copy_from_slice(&0x20u16.to_le_bytes()); // archive attribute
    for ts_off in [8usize, 12, 16] {
        set[ts_off..ts_off + 4].copy_from_slice(&TIMESTAMP.to_le_bytes());
    }
    set[22] = 0x80; // create UTC offset: valid, +0
    set[23] = 0x80; // modify UTC offset
    set[24] = 0x80; // access UTC offset

    // Stream extension (0xC0). Name hash is over the UP-CASED name; ours
    // is hashed pre-uppercased to satisfy that without an up-case table.
    let upcased: Vec<u16> = name_utf16
        .iter()
        .map(|&c| (c as u8).to_ascii_uppercase() as u16)
        .collect();
    let s = &mut set[32..64];
    s[0] = ET_STREAM;
    s[1] = if clusters > 0 {
        FLAG_ALLOC_POSSIBLE | FLAG_NO_FAT_CHAIN
    } else {
        FLAG_ALLOC_POSSIBLE
    };
    s[3] = name.len() as u8;
    s[4..6].copy_from_slice(&name_hash(&upcased).to_le_bytes());
    s[8..16].copy_from_slice(&(data.len() as u64).to_le_bytes()); // valid data length
    s[20..24].copy_from_slice(&first_cluster.to_le_bytes());
    s[24..32].copy_from_slice(&(data.len() as u64).to_le_bytes()); // data length

    // FileName entries (0xC1), 15 UTF-16 chars each
    for n in 0..name_entries {
        let e = &mut set[(2 + n) * DIR_ENTRY..(3 + n) * DIR_ENTRY];
        e[0] = ET_NAME;
        for i in 0..15 {
            let idx = n * 15 + i;
            if idx < name_utf16.len() {
                e[2 + i * 2..4 + i * 2].copy_from_slice(&name_utf16[idx].to_le_bytes());
            }
        }
    }

    let checksum = entry_set_checksum(&set);
    set[2..4].copy_from_slice(&checksum.to_le_bytes());

    // 3. Append the set at the root directory's end-of-directory marker
    let mut cluster_buf = vec![0u8; cbytes];
    let mut cluster = vol.root_cluster;
    loop {
        if cluster < 2 || !read_cluster(vol, cluster, &mut cluster_buf) {
            return Err("root dir read failed");
        }
        if let Some(pos) = cluster_buf
            .chunks_exact(DIR_ENTRY)
            .position(|e| e[0] == ET_END)
        {
            let off = pos * DIR_ENTRY;
            if off + set_len > cbytes {
                return Err("no room in root directory cluster");
            }
            cluster_buf[off..off + set_len].copy_from_slice(&set);
            // Entries after the set are already zero (end-of-directory)

            // Write back the touched sectors
            let lba = vol.cluster_lba(cluster);
            let first_sec = off / SECTOR;
            let last_sec = (off + set_len - 1) / SECTOR;
            for sidx in first_sec..=last_sec {
                let mut sec = [0u8; SECTOR];
                sec.copy_from_slice(&cluster_buf[sidx * SECTOR..(sidx + 1) * SECTOR]);
                if !write_sector(lba + sidx as u64, &mut sec) {
                    return Err("dir write failed");
                }
            }
            return Ok(());
        }
        let next = fat_entry(vol, cluster);
        if next < 2 || next == FAT_EOF {
            return Err("root directory is full");
        }
        cluster = next;
    }
}

// ========================================================================
// Boot demo
// ========================================================================

/// Mount + list + read + create + verify. Run at boot after virtio init.
pub fn demo() {
    if !mount() {
        return;
    }
    list_root();

    // Read a file seeded from the host
    if let Some(data) = read_file("README.TXT") {
        crate::serial::write_str("[FS] README.TXT: ");
        for &b in data.iter().take(120) {
            if b.is_ascii_graphic() || b == b' ' {
                let s = [b];
                crate::serial::write_str(core::str::from_utf8(&s).unwrap_or("?"));
            }
        }
        crate::serial::write_str("\n");
    }

    // Create a file — or, on later boots, find the one we created before
    const OURS: &str = "MMURTL.TXT";
    const CONTENT: &[u8] = b"Written by the MMURTL/RS exFAT driver. \
If you can read this from the host, the write path works end to end.\n";

    match read_file(OURS) {
        Some(data) => {
            crate::serial::write_str("[FS] MMURTL.TXT persisted from a previous boot (");
            crate::serial::write_dec(data.len() as u64);
            crate::serial::write_str(" bytes) — write path verified across reboots\n");
        }
        None => match create_file(OURS, CONTENT) {
            Ok(()) => match read_file(OURS) {
                Some(back) if back == CONTENT => {
                    crate::serial::write_str("[FS] Created MMURTL.TXT (");
                    crate::serial::write_dec(CONTENT.len() as u64);
                    crate::serial::write_str(" bytes), read-back verified\n");
                }
                _ => crate::serial::write_line("[FS] Created MMURTL.TXT but READ-BACK FAILED"),
            },
            Err(e) => {
                crate::serial::write_str("[FS] create_file failed: ");
                crate::serial::write_line(e);
            }
        },
    }

    list_root();
}
