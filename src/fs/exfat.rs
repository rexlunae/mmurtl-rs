//! exFAT filesystem driver on virtio-blk.
//!
//! Implements the on-disk format from the Microsoft exFAT specification:
//!   - Boot sector: geometry, FAT offset, cluster heap, root directory
//!   - Directory entry sets: File (0x85) + Stream Extension (0xC0) +
//!     FileName (0xC1), with the rotate-right entry-set checksum
//!   - Cluster chains via the FAT, plus the NoFatChain contiguous fast path
//!   - Allocation Bitmap (0x81) for cluster accounting
//!
//! Supported operations (all paths may traverse subdirectories):
//!   mount, list_dir, read_file, create_file, write_file (overwrite),
//!   append_file, delete (files and empty directories), mkdir
//!
//! Created files use contiguous NoFatChain allocation (spec-conformant:
//! the FAT is not consulted for such files). Created directories are
//! FAT-chained. The root directory grows on demand by extending its FAT
//! chain; other directories have fixed capacity (~40 files per 4 KiB
//! cluster) until parent-entry resizing is implemented.
//!
//! Remaining limitations: ASCII names (≤ 255 chars), no rename, no
//! sparse/fragmented writes (overwrites reallocate contiguously),
//! 512-byte sectors.

use alloc::string::String;
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
/// InUse bit — clearing it turns an entry into a deleted/unused one
const ET_IN_USE: u8 = 0x80;

// Stream extension GeneralSecondaryFlags
const FLAG_ALLOC_POSSIBLE: u8 = 0x01;
const FLAG_NO_FAT_CHAIN: u8 = 0x02;

/// File attribute bits
const ATTR_DIRECTORY: u16 = 0x10;
const ATTR_ARCHIVE: u16 = 0x20;

/// FAT end-of-chain marker
const FAT_EOF: u32 = 0xFFFF_FFFF;

/// Fixed timestamp for created entries: 2026-07-17 08:00:00 UTC
const TIMESTAMP: u32 = ((2026 - 1980) << 25) | (7 << 21) | (17 << 16) | (8 << 11);

// ========================================================================
// Volume state
// ========================================================================

struct Volume {
    sectors_per_cluster: u32,
    fat_lba: u64,
    heap_lba: u64,
    cluster_count: u32,
    root_cluster: u32,
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

/// Read one FAT entry. Returns None on I/O failure — callers must NOT
/// treat a failed read as end-of-chain (a zeroed buffer would decode as
/// 0, silently truncating the chain).
fn fat_read(vol: &Volume, cluster: u32) -> Option<u32> {
    let byte_off = cluster as u64 * 4;
    let mut buf = [0u8; SECTOR];
    if !read_sector(vol.fat_lba + byte_off / SECTOR as u64, &mut buf) {
        return None;
    }
    Some(le32(&buf[(byte_off % SECTOR as u64) as usize..]))
}

fn fat_write(vol: &Volume, cluster: u32, value: u32) -> bool {
    let byte_off = cluster as u64 * 4;
    let lba = vol.fat_lba + byte_off / SECTOR as u64;
    let off = (byte_off % SECTOR as u64) as usize;
    let mut buf = [0u8; SECTOR];
    if !read_sector(lba, &mut buf) {
        return false;
    }
    buf[off..off + 4].copy_from_slice(&value.to_le_bytes());
    write_sector(lba, &mut buf)
}

fn read_cluster(vol: &Volume, cluster: u32, out: &mut [u8]) -> bool {
    let lba = vol.cluster_lba(cluster);
    for s in 0..vol.sectors_per_cluster as u64 {
        let mut buf = [0u8; SECTOR];
        if !read_sector(lba + s, &mut buf) {
            return false;
        }
        out[s as usize * SECTOR..(s as usize + 1) * SECTOR].copy_from_slice(&buf);
    }
    true
}

fn write_cluster(vol: &Volume, cluster: u32, data: &[u8]) -> bool {
    let lba = vol.cluster_lba(cluster);
    for s in 0..vol.sectors_per_cluster as usize {
        let mut buf = [0u8; SECTOR];
        let start = s * SECTOR;
        if start < data.len() {
            let take = (data.len() - start).min(SECTOR);
            buf[..take].copy_from_slice(&data[start..start + take]);
        }
        if !write_sector(lba + s as u64, &mut buf) {
            return false;
        }
    }
    true
}

fn zero_cluster(vol: &Volume, cluster: u32) -> bool {
    write_cluster(vol, cluster, &[])
}

/// Rotate-right checksum over an entry set (bytes 2-3 of the first entry
/// — the checksum field itself — are skipped)
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

fn names_equal_ascii(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .all(|(x, y)| x.to_ascii_uppercase() == y.to_ascii_uppercase())
}

// ========================================================================
// Cluster chains
// ========================================================================

/// Collect the cluster list of a file or directory.
/// `no_fat_chain` means contiguous clusters covering `data_length` bytes.
///
/// Returns None if a FAT read fails mid-walk — the chain is then unknown,
/// which is an I/O error, not a shorter chain.
fn collect_chain(
    vol: &Volume,
    first: u32,
    no_fat_chain: bool,
    data_length: u64,
) -> Option<Vec<u32>> {
    let mut chain = Vec::new();
    if first < 2 {
        return Some(chain);
    }
    if no_fat_chain {
        let cbytes = vol.cluster_bytes() as u64;
        let n = ((data_length + cbytes - 1) / cbytes) as u32;
        for c in first..first + n {
            chain.push(c);
        }
    } else {
        let mut c = first;
        for _ in 0..=vol.cluster_count {
            chain.push(c);
            let next = fat_read(vol, c)?; // I/O failure aborts the walk
            if next < 2 || next == FAT_EOF || next > vol.cluster_count + 1 {
                break;
            }
            c = next;
        }
    }
    Some(chain)
}

// ========================================================================
// Allocation bitmap
// ========================================================================

/// Allocate `count` contiguous clusters. Bit N of the bitmap = cluster N+2.
fn bitmap_alloc(vol: &Volume, count: usize) -> Option<u32> {
    let bitmap_lba = vol.cluster_lba(vol.bitmap_cluster);
    let sectors = (vol.bitmap_bytes as usize + SECTOR - 1) / SECTOR;

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

    let clusters: Vec<u32> = (0..count).map(|i| (start_bit + i) as u32 + 2).collect();
    if !bitmap_set(vol, &clusters, true) {
        return None;
    }
    Some(start_bit as u32 + 2)
}

/// Set or clear the bitmap bits for a list of clusters
fn bitmap_set(vol: &Volume, clusters: &[u32], allocated: bool) -> bool {
    let bitmap_lba = vol.cluster_lba(vol.bitmap_cluster);
    let mut buf = [0u8; SECTOR];
    let mut loaded_sector: Option<u64> = None;

    // Process sorted so each bitmap sector is loaded/flushed once per run
    let mut sorted: Vec<u32> = clusters.to_vec();
    sorted.sort_unstable();

    for &cluster in &sorted {
        if cluster < 2 {
            continue;
        }
        let bit = (cluster - 2) as usize;
        let sector = (bit / (SECTOR * 8)) as u64;
        if loaded_sector != Some(sector) {
            if let Some(prev) = loaded_sector {
                if !write_sector(bitmap_lba + prev, &mut buf) {
                    return false;
                }
            }
            if !read_sector(bitmap_lba + sector, &mut buf) {
                return false;
            }
            loaded_sector = Some(sector);
        }
        let idx = (bit % (SECTOR * 8)) / 8;
        let mask = 1 << (bit % 8);
        if allocated {
            buf[idx] |= mask;
        } else {
            buf[idx] &= !mask;
        }
    }
    if let Some(prev) = loaded_sector {
        if !write_sector(bitmap_lba + prev, &mut buf) {
            return false;
        }
    }
    true
}

/// Free a file's clusters: clear bitmap bits and, for FAT-chained files,
/// zero the stale FAT entries.
fn free_clusters(vol: &Volume, first: u32, no_fat_chain: bool, data_length: u64) {
    // On a FAT I/O failure the chain is unknown: free nothing. Leaking
    // clusters is recoverable (fsck); freeing the wrong ones is not.
    let chain = match collect_chain(vol, first, no_fat_chain, data_length) {
        Some(c) => c,
        None => return,
    };
    if chain.is_empty() {
        return;
    }
    bitmap_set(vol, &chain, false);
    if !no_fat_chain {
        for &c in &chain {
            fat_write(vol, c, 0);
        }
    }
}

// ========================================================================
// Directories: loading, parsing, storing
// ========================================================================

/// Reference to a directory (root or subdirectory)
#[derive(Clone, Copy)]
struct DirRef {
    first_cluster: u32,
    no_fat_chain: bool,
    data_length: u64,
    is_root: bool,
}

/// A parsed file/directory entry set within a loaded directory
struct DirFile {
    name: String,
    attributes: u16,
    first_cluster: u32,
    data_length: u64,
    no_fat_chain: bool,
    /// Linear byte offset of the 0x85 entry within the loaded directory
    set_offset: usize,
    /// Total entries in the set (1 + secondary count)
    set_entries: usize,
}

impl DirFile {
    fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }
}

/// Load a directory's cluster chain and full contents into memory.
/// None on I/O failure or an empty/invalid chain.
fn load_dir(vol: &Volume, dir: &DirRef) -> Option<(Vec<u32>, Vec<u8>)> {
    let chain = collect_chain(vol, dir.first_cluster, dir.no_fat_chain, dir.data_length)?;
    if chain.is_empty() {
        return None;
    }
    let cbytes = vol.cluster_bytes();
    let mut data = vec![0u8; chain.len() * cbytes];
    for (i, &c) in chain.iter().enumerate() {
        if !read_cluster(vol, c, &mut data[i * cbytes..(i + 1) * cbytes]) {
            return None;
        }
    }
    Some((chain, data))
}

/// Write back the byte range [start, start+len) of a loaded directory
fn store_dir_range(vol: &Volume, chain: &[u32], data: &[u8], start: usize, len: usize) -> bool {
    let cbytes = vol.cluster_bytes();
    let first_cl = start / cbytes;
    let last_cl = (start + len - 1) / cbytes;
    for ci in first_cl..=last_cl {
        let cluster = chain[ci];
        let lba = vol.cluster_lba(cluster);
        // Write only the touched sectors of this cluster
        let cl_start = ci * cbytes;
        let touch_start = start.max(cl_start);
        let touch_end = (start + len).min(cl_start + cbytes);
        let first_sec = (touch_start - cl_start) / SECTOR;
        let last_sec = (touch_end - 1 - cl_start) / SECTOR;
        for s in first_sec..=last_sec {
            let mut buf = [0u8; SECTOR];
            let off = cl_start + s * SECTOR;
            buf.copy_from_slice(&data[off..off + SECTOR]);
            if !write_sector(lba + s as u64, &mut buf) {
                return false;
            }
        }
    }
    true
}

/// Parse all in-use file/directory entry sets from loaded directory data
fn parse_entries(data: &[u8]) -> Vec<DirFile> {
    let mut out = Vec::new();
    let mut current: Option<DirFile> = None;
    let mut secondaries_left = 0u8;

    for (idx, e) in data.chunks_exact(DIR_ENTRY).enumerate() {
        match e[0] {
            ET_END => break,
            ET_FILE => {
                current = Some(DirFile {
                    name: String::new(),
                    attributes: le16(&e[4..]),
                    first_cluster: 0,
                    data_length: 0,
                    no_fat_chain: false,
                    set_offset: idx * DIR_ENTRY,
                    set_entries: 1 + e[1] as usize,
                });
                secondaries_left = e[1];
            }
            t if t & ET_IN_USE != 0 && current.is_some() => {
                let df = current.as_mut().unwrap();
                match t {
                    ET_STREAM => {
                        df.no_fat_chain = e[1] & FLAG_NO_FAT_CHAIN != 0;
                        df.first_cluster = le32(&e[20..]);
                        df.data_length = le64(&e[24..]);
                    }
                    ET_NAME => {
                        for ch in e[2..32].chunks_exact(2) {
                            let c = le16(ch);
                            if c != 0 && df.name.len() < 255 {
                                df.name.push(if c < 0x80 { c as u8 as char } else { '?' });
                            }
                        }
                    }
                    _ => {}
                }
                if secondaries_left > 0 {
                    secondaries_left -= 1;
                    if secondaries_left == 0 {
                        out.push(current.take().unwrap());
                    }
                }
            }
            _ => {
                // Deleted/unknown entry interrupts any in-progress set
                current = None;
                secondaries_left = 0;
            }
        }
    }
    out
}

// ========================================================================
// Path resolution
// ========================================================================

fn root_ref(vol: &Volume) -> DirRef {
    DirRef {
        first_cluster: vol.root_cluster,
        no_fat_chain: false,
        data_length: 0,
        is_root: true,
    }
}

fn dir_ref_of(f: &DirFile) -> DirRef {
    DirRef {
        first_cluster: f.first_cluster,
        no_fat_chain: f.no_fat_chain,
        data_length: f.data_length,
        is_root: false,
    }
}

/// Split "A/B/C.TXT" into components, ignoring leading/duplicate slashes
fn components(path: &str) -> Vec<&str> {
    path.split('/').filter(|c| !c.is_empty()).collect()
}

/// Resolve the directory containing the last path component.
/// Returns (parent dir, final component name).
fn resolve_parent<'p>(vol: &Volume, path: &'p str) -> Option<(DirRef, &'p str)> {
    let comps = components(path);
    let (last, dirs) = comps.split_last()?;
    let mut cur = root_ref(vol);
    for comp in dirs {
        let (_, data) = load_dir(vol, &cur)?;
        let entries = parse_entries(&data);
        let found = entries
            .iter()
            .find(|f| f.is_dir() && names_equal_ascii(&f.name, comp))?;
        cur = dir_ref_of(found);
    }
    Some((cur, last))
}

/// Look up a path's final entry. Returns (parent, entry).
fn lookup(vol: &Volume, path: &str) -> Option<(DirRef, DirFile)> {
    let (parent, name) = resolve_parent(vol, path)?;
    let (_, data) = load_dir(vol, &parent)?;
    parse_entries(&data)
        .into_iter()
        .find(|f| names_equal_ascii(&f.name, name))
        .map(|f| (parent, f))
}

// ========================================================================
// Mount
// ========================================================================

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
    if bs[108] != 9 {
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
        let root = root_ref(&vol);
        let chain = match collect_chain(&vol, root.first_cluster, false, 0) {
            Some(c) => c,
            None => {
                crate::serial::write_line("[FS] I/O error walking root FAT chain — not mounted");
                return false;
            }
        };
        let cbytes = vol.cluster_bytes();
        let mut buf = vec![0u8; cbytes];
        'outer: for &cluster in &chain {
            if !read_cluster(&vol, cluster, &mut buf) {
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
// Public read API
// ========================================================================

/// Print a directory listing ("" or "/" for root)
pub fn list_dir(path: &str) {
    let guard = VOLUME.lock();
    let vol = match guard.as_ref() {
        Some(v) => v,
        None => return,
    };

    let dir = if components(path).is_empty() {
        Some(root_ref(vol))
    } else {
        lookup(vol, path).filter(|(_, f)| f.is_dir()).map(|(_, f)| dir_ref_of(&f))
    };
    let dir = match dir {
        Some(d) => d,
        None => {
            crate::serial::write_line("[FS] list_dir: no such directory");
            return;
        }
    };

    crate::serial::write_str("[FS] Listing /");
    crate::serial::write_str(path.trim_matches('/'));
    crate::serial::write_str(":\n");

    let entries = match load_dir(vol, &dir) {
        Some((_, data)) => parse_entries(&data),
        None => return,
    };
    if entries.is_empty() {
        crate::serial::write_line("[FS]   (empty)");
    }
    for f in entries {
        crate::serial::write_str("[FS]   ");
        crate::serial::write_str(&f.name);
        if f.is_dir() {
            crate::serial::write_str("/");
        } else {
            crate::serial::write_str("  (");
            crate::serial::write_dec(f.data_length);
            crate::serial::write_str(" bytes)");
        }
        crate::serial::write_str("\n");
    }
}

/// Read a whole file by path
pub fn read_file(path: &str) -> Option<Vec<u8>> {
    let guard = VOLUME.lock();
    let vol = guard.as_ref()?;
    let (_, f) = lookup(vol, path)?;
    if f.is_dir() {
        return None;
    }
    read_file_inner(vol, &f)
}

fn read_file_inner(vol: &Volume, f: &DirFile) -> Option<Vec<u8>> {
    if f.data_length == 0 {
        return Some(Vec::new());
    }
    let chain = collect_chain(vol, f.first_cluster, f.no_fat_chain, f.data_length)?;
    let cbytes = vol.cluster_bytes();
    if chain.len() * cbytes < f.data_length as usize {
        return None; // chain shorter than data_length
    }
    let mut data = Vec::with_capacity(f.data_length as usize);
    let mut cluster_buf = vec![0u8; cbytes];
    let mut remaining = f.data_length as usize;
    for &c in &chain {
        if remaining == 0 {
            break;
        }
        if !read_cluster(vol, c, &mut cluster_buf) {
            return None;
        }
        let take = remaining.min(cbytes);
        data.extend_from_slice(&cluster_buf[..take]);
        remaining -= take;
    }
    Some(data)
}

// ========================================================================
// Entry-set construction and placement
// ========================================================================

/// Build a File + Stream + FileName entry set
fn build_entry_set(
    name: &str,
    attributes: u16,
    flags: u8,
    first_cluster: u32,
    data_length: u64,
) -> Vec<u8> {
    let name_utf16: Vec<u16> = name.bytes().map(|b| b as u16).collect();
    let name_entries = (name.len() + 14) / 15;
    let set_len = (2 + name_entries) * DIR_ENTRY;
    let mut set = vec![0u8; set_len];

    set[0] = ET_FILE;
    set[1] = (1 + name_entries) as u8; // secondary count
    set[4..6].copy_from_slice(&attributes.to_le_bytes());
    for ts_off in [8usize, 12, 16] {
        set[ts_off..ts_off + 4].copy_from_slice(&TIMESTAMP.to_le_bytes());
    }
    set[22] = 0x80; // create/modify/access UTC offsets: valid, +0
    set[23] = 0x80;
    set[24] = 0x80;

    let upcased: Vec<u16> = name_utf16
        .iter()
        .map(|&c| (c as u8).to_ascii_uppercase() as u16)
        .collect();
    let s = &mut set[32..64];
    s[0] = ET_STREAM;
    s[1] = flags;
    s[3] = name.len() as u8;
    s[4..6].copy_from_slice(&name_hash(&upcased).to_le_bytes());
    s[8..16].copy_from_slice(&data_length.to_le_bytes()); // valid data length
    s[20..24].copy_from_slice(&first_cluster.to_le_bytes());
    s[24..32].copy_from_slice(&data_length.to_le_bytes());

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
    set
}

/// Place an entry set into a directory: reuse a run of deleted entries,
/// append at the end marker, or (root only) grow the directory's FAT
/// chain by a cluster.
fn place_entry_set(vol: &Volume, dir: &DirRef, set: &[u8]) -> Result<(), &'static str> {
    let (mut chain, mut data) = load_dir(vol, dir).ok_or("directory read failed")?;
    let needed = set.len() / DIR_ENTRY;
    let cbytes = vol.cluster_bytes();

    // Scan for a home: a run of deleted entries, or the end marker
    let mut run_start = 0usize;
    let mut run_len = 0usize;
    let mut place_at: Option<usize> = None;
    let mut end_marker: Option<usize> = None;

    for (idx, e) in data.chunks_exact(DIR_ENTRY).enumerate() {
        if e[0] == ET_END {
            end_marker = Some(idx);
            break;
        }
        if e[0] & ET_IN_USE == 0 {
            if run_len == 0 {
                run_start = idx;
            }
            run_len += 1;
            if run_len == needed {
                place_at = Some(run_start);
                break;
            }
        } else {
            run_len = 0;
        }
    }

    let offset = match place_at {
        Some(idx) => idx * DIR_ENTRY,
        None => {
            let end = end_marker.map(|e| e * DIR_ENTRY).unwrap_or(data.len());
            // Grow if the set (plus room for the end marker's invariant)
            // doesn't fit in the existing clusters
            while end + set.len() > data.len() {
                if !dir.is_root {
                    return Err("directory full");
                }
                let new_cluster = bitmap_alloc(vol, 1).ok_or("disk full")?;
                // Prepare the new cluster fully (zeroed, FAT terminator)
                // BEFORE linking it, so a failure here just needs the
                // allocation undone and never leaves a half-linked chain.
                if !zero_cluster(vol, new_cluster) || !fat_write(vol, new_cluster, FAT_EOF) {
                    fat_write(vol, new_cluster, 0);
                    bitmap_set(vol, &[new_cluster], false);
                    return Err("cluster init failed");
                }
                let tail = *chain.last().unwrap();
                if !fat_write(vol, tail, new_cluster) {
                    fat_write(vol, new_cluster, 0);
                    bitmap_set(vol, &[new_cluster], false);
                    return Err("FAT update failed");
                }
                // From here the cluster is a legitimate (empty) part of
                // the directory — a later failure leaves a valid, merely
                // larger directory, not an inconsistency.
                chain.push(new_cluster);
                data.resize(chain.len() * cbytes, 0);
            }
            end
        }
    };

    data[offset..offset + set.len()].copy_from_slice(set);
    if !store_dir_range(vol, &chain, &data, offset, set.len()) {
        return Err("directory write failed");
    }
    Ok(())
}

/// Rewrite a file's stream-extension fields in place (allocation, size,
/// flags) and refresh the entry-set checksum.
fn update_stream_entry(
    vol: &Volume,
    dir: &DirRef,
    file: &DirFile,
    flags: u8,
    first_cluster: u32,
    data_length: u64,
) -> Result<(), &'static str> {
    let (chain, mut data) = load_dir(vol, dir).ok_or("directory read failed")?;
    let off = file.set_offset;
    let set_len = file.set_entries * DIR_ENTRY;
    if off + set_len > data.len() || data[off] != ET_FILE {
        return Err("stale directory entry");
    }

    let s = &mut data[off + 32..off + 64];
    if s[0] != ET_STREAM {
        return Err("missing stream entry");
    }
    s[1] = flags;
    s[8..16].copy_from_slice(&data_length.to_le_bytes());
    s[20..24].copy_from_slice(&first_cluster.to_le_bytes());
    s[24..32].copy_from_slice(&data_length.to_le_bytes());

    let checksum = entry_set_checksum(&data[off..off + set_len]);
    data[off + 2..off + 4].copy_from_slice(&checksum.to_le_bytes());

    if !store_dir_range(vol, &chain, &data, off, set_len) {
        return Err("directory write failed");
    }
    Ok(())
}

/// Free a contiguous run previously produced by `write_data_contiguous`
/// (rollback for failed create/overwrite paths). No-op for empty files.
fn rollback_contiguous(vol: &Volume, first: u32, data_len: usize) {
    if first >= 2 && data_len > 0 {
        free_clusters(vol, first, true, data_len as u64);
    }
}

/// Allocate contiguous clusters for `data` and write it. Returns the
/// first cluster (0 for empty data). On failure the allocation is rolled
/// back — no bitmap bits stay set without a directory entry to own them.
fn write_data_contiguous(vol: &Volume, data: &[u8]) -> Result<u32, &'static str> {
    if data.is_empty() {
        return Ok(0);
    }
    let cbytes = vol.cluster_bytes();
    let clusters = (data.len() + cbytes - 1) / cbytes;
    let first = bitmap_alloc(vol, clusters).ok_or("no contiguous space")?;
    for i in 0..clusters {
        let chunk_start = i * cbytes;
        let chunk_end = (chunk_start + cbytes).min(data.len());
        if !write_cluster(vol, first + i as u32, &data[chunk_start..chunk_end]) {
            rollback_contiguous(vol, first, data.len());
            return Err("data write failed");
        }
    }
    Ok(first)
}

fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > 255 || !name.is_ascii() {
        return Err("name must be ASCII, 1-255 chars");
    }
    if name.contains('/') || name.contains('\\') {
        return Err("name may not contain slashes");
    }
    Ok(())
}

// ========================================================================
// Public write API
// ========================================================================

/// Create a new file at `path` with `data` (fails if it exists)
pub fn create_file(path: &str, data: &[u8]) -> Result<(), &'static str> {
    let guard = VOLUME.lock();
    let vol = guard.as_ref().ok_or("not mounted")?;
    let (parent, name) = resolve_parent(vol, path).ok_or("no such directory")?;
    validate_name(name)?;

    let (_, pdata) = load_dir(vol, &parent).ok_or("directory read failed")?;
    if parse_entries(&pdata)
        .iter()
        .any(|f| names_equal_ascii(&f.name, name))
    {
        return Err("already exists");
    }

    let first = write_data_contiguous(vol, data)?;
    let flags = if data.is_empty() {
        FLAG_ALLOC_POSSIBLE
    } else {
        FLAG_ALLOC_POSSIBLE | FLAG_NO_FAT_CHAIN
    };
    let set = build_entry_set(name, ATTR_ARCHIVE, flags, first, data.len() as u64);
    if let Err(e) = place_entry_set(vol, &parent, &set) {
        // No directory entry references the clusters — free them again
        // so the bitmap stays consistent.
        rollback_contiguous(vol, first, data.len());
        return Err(e);
    }
    Ok(())
}

/// Overwrite `path` with `data`, creating it if absent
pub fn write_file(path: &str, data: &[u8]) -> Result<(), &'static str> {
    {
        let guard = VOLUME.lock();
        let vol = guard.as_ref().ok_or("not mounted")?;
        if let Some((parent, f)) = lookup(vol, path) {
            if f.is_dir() {
                return Err("is a directory");
            }
            // Order matters: write the NEW allocation first, then repoint
            // the entry, and only then free the old clusters. Any failure
            // before the entry update leaves the old file fully intact;
            // a failed entry update rolls the new allocation back.
            let first = write_data_contiguous(vol, data)?;
            let flags = if data.is_empty() {
                FLAG_ALLOC_POSSIBLE
            } else {
                FLAG_ALLOC_POSSIBLE | FLAG_NO_FAT_CHAIN
            };
            if let Err(e) = update_stream_entry(vol, &parent, &f, flags, first, data.len() as u64)
            {
                rollback_contiguous(vol, first, data.len());
                return Err(e);
            }
            free_clusters(vol, f.first_cluster, f.no_fat_chain, f.data_length);
            return Ok(());
        }
    }
    create_file(path, data)
}

/// Append `data` to `path`, creating it if absent.
///
/// "File absent" and "file unreadable" are deliberately distinct here: if
/// the file exists but its contents can't be read (transient I/O error),
/// appending must fail rather than silently rewrite the file with only
/// the new bytes.
pub fn append_file(path: &str, data: &[u8]) -> Result<(), &'static str> {
    let existing = {
        let guard = VOLUME.lock();
        let vol = guard.as_ref().ok_or("not mounted")?;
        match lookup(vol, path) {
            Some((_, f)) if f.is_dir() => return Err("is a directory"),
            Some((_, f)) => Some(read_file_inner(vol, &f).ok_or("read failed")?),
            None => None,
        }
    };
    let mut combined = existing.unwrap_or_default();
    combined.extend_from_slice(data);
    write_file(path, &combined)
}

/// Delete a file or an empty directory
pub fn delete(path: &str) -> Result<(), &'static str> {
    let guard = VOLUME.lock();
    let vol = guard.as_ref().ok_or("not mounted")?;
    let (parent, f) = lookup(vol, path).ok_or("no such file")?;

    if f.is_dir() {
        let (_, ddata) = load_dir(vol, &dir_ref_of(&f)).ok_or("directory read failed")?;
        if !parse_entries(&ddata).is_empty() {
            return Err("directory not empty");
        }
    }

    // Order matters: unlink the directory entries FIRST, then free the
    // clusters. If the entry write fails, nothing has changed; if the
    // cluster free fails afterwards, the worst case is leaked (orphaned)
    // clusters — never a live entry pointing at freed space.
    let (chain, mut data) = load_dir(vol, &parent).ok_or("directory read failed")?;
    let off = f.set_offset;
    let set_len = f.set_entries * DIR_ENTRY;
    if off + set_len > data.len() || data[off] != ET_FILE {
        return Err("stale directory entry");
    }
    for i in 0..f.set_entries {
        data[off + i * DIR_ENTRY] &= !ET_IN_USE;
    }
    if !store_dir_range(vol, &chain, &data, off, set_len) {
        return Err("directory write failed");
    }

    free_clusters(vol, f.first_cluster, f.no_fat_chain, f.data_length);
    Ok(())
}

/// Create a directory at `path` (single new component)
pub fn mkdir(path: &str) -> Result<(), &'static str> {
    let guard = VOLUME.lock();
    let vol = guard.as_ref().ok_or("not mounted")?;
    let (parent, name) = resolve_parent(vol, path).ok_or("no such directory")?;
    validate_name(name)?;

    let (_, pdata) = load_dir(vol, &parent).ok_or("directory read failed")?;
    if parse_entries(&pdata)
        .iter()
        .any(|f| names_equal_ascii(&f.name, name))
    {
        return Err("already exists");
    }

    // One zeroed, FAT-chained cluster (chained keeps growth possible)
    let cluster = bitmap_alloc(vol, 1).ok_or("disk full")?;
    let rollback = |vol: &Volume| {
        fat_write(vol, cluster, 0);
        bitmap_set(vol, &[cluster], false);
    };
    if !zero_cluster(vol, cluster) || !fat_write(vol, cluster, FAT_EOF) {
        rollback(vol);
        return Err("cluster init failed");
    }

    let set = build_entry_set(
        name,
        ATTR_DIRECTORY,
        FLAG_ALLOC_POSSIBLE,
        cluster,
        vol.cluster_bytes() as u64,
    );
    if let Err(e) = place_entry_set(vol, &parent, &set) {
        rollback(vol);
        return Err(e);
    }
    Ok(())
}

// ========================================================================
// Boot demo
// ========================================================================

fn print_text(prefix: &str, data: &[u8]) {
    crate::serial::write_str(prefix);
    for &b in data.iter().take(200) {
        if b == b'\n' {
            crate::serial::write_str("\\n");
        } else if b.is_ascii_graphic() || b == b' ' {
            let s = [b];
            crate::serial::write_str(core::str::from_utf8(&s).unwrap_or("?"));
        }
    }
    crate::serial::write_str("\n");
}

/// Exercise the full API at boot: read, create, mkdir, append (grows
/// across reboots), delete, nested host files.
pub fn demo() {
    if !mount() {
        return;
    }
    list_dir("/");

    if let Some(data) = read_file("README.TXT") {
        print_text("[FS] README.TXT: ", &data);
    }
    // A file the host seeded inside a subdirectory (foreign dir traversal)
    if let Some(data) = read_file("HOST_DIR/HELLO.TXT") {
        print_text("[FS] HOST_DIR/HELLO.TXT: ", &data);
    }

    // mkdir — idempotent across boots
    match mkdir("DOCS") {
        Ok(()) => crate::serial::write_line("[FS] mkdir DOCS: created"),
        Err("already exists") => crate::serial::write_line("[FS] mkdir DOCS: already exists"),
        Err(e) => {
            crate::serial::write_str("[FS] mkdir DOCS failed: ");
            crate::serial::write_line(e);
        }
    }

    // Append one line per boot — persistence + overwrite/realloc exercise
    match append_file("DOCS/BOOTLOG.TXT", b"MMURTL/RS booted again.\n") {
        Ok(()) => {
            if let Some(data) = read_file("DOCS/BOOTLOG.TXT") {
                let boots = data.iter().filter(|&&b| b == b'\n').count();
                crate::serial::write_str("[FS] DOCS/BOOTLOG.TXT now records ");
                crate::serial::write_dec(boots as u64);
                crate::serial::write_str(" boot(s)\n");
            }
        }
        Err(e) => {
            crate::serial::write_str("[FS] append failed: ");
            crate::serial::write_line(e);
        }
    }

    // Create + delete round trip
    let _ = delete("TEMP.TXT"); // clean up any prior run
    match create_file("TEMP.TXT", b"transient") {
        Ok(()) => match delete("TEMP.TXT") {
            Ok(()) if read_file("TEMP.TXT").is_none() => {
                crate::serial::write_line("[FS] create+delete TEMP.TXT: OK (gone after delete)")
            }
            Ok(()) => crate::serial::write_line("[FS] delete claimed OK but file still readable!"),
            Err(e) => {
                crate::serial::write_str("[FS] delete failed: ");
                crate::serial::write_line(e);
            }
        },
        Err(e) => {
            crate::serial::write_str("[FS] create TEMP.TXT failed: ");
            crate::serial::write_line(e);
        }
    }

    // Overwrite with different-size content, verify
    let long = b"This larger content replaces the short one and reallocates clusters.";
    if write_file("DOCS/OVERWRITE.TXT", b"short").is_ok()
        && write_file("DOCS/OVERWRITE.TXT", long).is_ok()
        && read_file("DOCS/OVERWRITE.TXT").as_deref() == Some(long.as_slice())
    {
        crate::serial::write_line("[FS] overwrite DOCS/OVERWRITE.TXT: verified");
    } else {
        crate::serial::write_line("[FS] overwrite test FAILED");
    }

    // Stress: 50 creates force the root directory to outgrow its first
    // cluster (FAT-chain extension); 50 deletes leave reusable entry runs
    {
        use alloc::format;
        let mut failed: Option<&'static str> = None;
        for i in 0..50 {
            if let Err(e) = create_file(&format!("STRESS{}.TXT", i), b"stress test payload") {
                failed = Some(e);
                break;
            }
        }
        if failed.is_none() {
            for i in 0..50 {
                if let Err(e) = delete(&format!("STRESS{}.TXT", i)) {
                    failed = Some(e);
                    break;
                }
            }
        }
        match failed {
            None => crate::serial::write_line(
                "[FS] stress: 50 creates (root dir grown) + 50 deletes OK",
            ),
            Some(e) => {
                crate::serial::write_str("[FS] stress FAILED: ");
                crate::serial::write_line(e);
            }
        }
    }

    list_dir("/");
    list_dir("/DOCS");
}
