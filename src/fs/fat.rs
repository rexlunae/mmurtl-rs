//! FAT12/16/32 with VFAT long filenames, on the current virtio block device.
//!
//! Format reference: Microsoft FAT Specification (2005), sections 3–7.
//! Supports 512-byte sectors, superfloppies, primary MBR partitions, and GPT.
//! Mutations update mirrored FATs (or FAT32's selected active FAT), invalidate
//! FSInfo hints, and publish new data before releasing old allocations. This
//! is not a journaled filesystem: an interrupted write can leave lost clusters.

use crate::virtio::blk;
use alloc::{string::String, vec, vec::Vec};
use spin::Mutex;

type Result<T> = core::result::Result<T, &'static str>;
const SECTOR: usize = 512;
const MAX_METADATA: usize = 8 * 1024 * 1024;
const MAX_FILE: usize = 16 * 1024 * 1024;
const LFN_POS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Fat12,
    Fat16,
    Fat32,
}
#[derive(Clone, Copy)]
struct Dir {
    cluster: u32,
    root: bool,
}
struct Record {
    name: String,
    alias: String,
    short: [u8; 11],
    attr: u8,
    cluster: u32,
    size: u32,
    offset: usize,
    start: usize,
}
impl Record {
    fn is_dir(&self) -> bool {
        self.attr & 0x10 != 0
    }
}

struct Volume {
    kind: Kind,
    fat_lba: u64,
    fat_sectors: u32,
    fats: u8,
    active: u8,
    mirrored: bool,
    data_lba: u64,
    spc: u32,
    clusters: u32,
    root_lba: u64,
    root_entries: usize,
    root_cluster: u32,
    fat: Vec<u8>,
    hint: u32,
    fsinfo: Option<u64>,
    backup_fsinfo: Option<u64>,
    hints_invalid: bool,
    free_count: u32,
    failed: bool,
}
static VOLUME: Mutex<Option<Volume>> = Mutex::new(None);

fn u16le(b: &[u8], n: usize) -> u16 {
    u16::from_le_bytes([b[n], b[n + 1]])
}
fn u32le(b: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(b[n..n + 4].try_into().unwrap())
}
fn u64le(b: &[u8], n: usize) -> u64 {
    u64::from_le_bytes(b[n..n + 8].try_into().unwrap())
}
fn sector(lba: u64) -> Result<[u8; SECTOR]> {
    let mut b = [0; SECTOR];
    if blk::read_sectors(lba, &mut b) {
        Ok(b)
    } else {
        Err("disk read failed")
    }
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320u32 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

impl Volume {
    fn root(&self) -> Dir {
        Dir {
            cluster: self.root_cluster,
            root: true,
        }
    }
    fn bytes(&self) -> usize {
        self.spc as usize * SECTOR
    }
    fn valid(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster < self.clusters + 2
    }
    fn lba(&self, cluster: u32) -> u64 {
        self.data_lba + (cluster as u64 - 2) * self.spc as u64
    }
    fn eof(&self) -> u32 {
        match self.kind {
            Kind::Fat12 => 0xfff,
            Kind::Fat16 => 0xffff,
            Kind::Fat32 => 0x0fffffff,
        }
    }
    fn is_eof(&self, value: u32) -> bool {
        value >= self.eof() - 7
    }
    fn check(&self) -> Result<()> {
        if self.failed {
            Err("filesystem stopped after a disk write failure; remount required")
        } else {
            Ok(())
        }
    }
    fn write_sector(&mut self, lba: u64, data: &[u8]) -> Result<()> {
        self.check()?;
        if data.len() != SECTOR {
            return Err("invalid sector write");
        }
        let mut buffer = [0; SECTOR];
        buffer.copy_from_slice(data);
        if !blk::write_sectors(lba, &mut buffer) {
            self.failed = true;
            return Err("disk write failed");
        }
        Ok(())
    }
    fn get(&self, cluster: u32) -> Result<u32> {
        if !self.valid(cluster) {
            return Err("invalid FAT cluster");
        }
        let n = cluster as usize;
        Ok(match self.kind {
            Kind::Fat12 => {
                let v = u16le(&self.fat, n + n / 2) as u32;
                if n & 1 == 0 {
                    v & 0xfff
                } else {
                    v >> 4
                }
            }
            Kind::Fat16 => u16le(&self.fat, n * 2) as u32,
            Kind::Fat32 => u32le(&self.fat, n * 4) & 0x0fffffff,
        })
    }
    fn set(&mut self, cluster: u32, value: u32) -> Result<()> {
        self.check()?;
        if !self.valid(cluster) {
            return Err("invalid FAT cluster");
        }
        let n = cluster as usize;
        let (off, len) = match self.kind {
            Kind::Fat12 => {
                let off = n + n / 2;
                let old = u16le(&self.fat, off);
                let new = if n & 1 == 0 {
                    (old & 0xf000) | (value as u16 & 0xfff)
                } else {
                    (old & 0xf) | ((value as u16 & 0xfff) << 4)
                };
                self.fat[off..off + 2].copy_from_slice(&new.to_le_bytes());
                (off, 2)
            }
            Kind::Fat16 => {
                self.fat[n * 2..n * 2 + 2].copy_from_slice(&(value as u16).to_le_bytes());
                (n * 2, 2)
            }
            Kind::Fat32 => {
                let old = u32le(&self.fat, n * 4);
                self.fat[n * 4..n * 4 + 4]
                    .copy_from_slice(&((old & 0xf0000000) | (value & 0x0fffffff)).to_le_bytes());
                (n * 4, 4)
            }
        };
        for copy in 0..self.fats {
            if !self.mirrored && copy != self.active {
                continue;
            }
            // FAT12's packed entry can straddle two sectors.
            for index in off / SECTOR..=(off + len - 1) / SECTOR {
                let mut data = [0; SECTOR];
                data.copy_from_slice(&self.fat[index * SECTOR..(index + 1) * SECTOR]);
                self.write_sector(
                    self.fat_lba + copy as u64 * self.fat_sectors as u64 + index as u64,
                    &data,
                )?;
            }
        }
        Ok(())
    }
    fn invalidate_hints(&mut self) -> Result<()> {
        if self.hints_invalid {
            return Ok(());
        }
        for lba in [self.fsinfo, self.backup_fsinfo].into_iter().flatten() {
            let mut data = sector(lba)?;
            if u32le(&data, 0) == 0x41615252
                && u32le(&data, 484) == 0x61417272
                && u32le(&data, 508) == 0xaa550000
            {
                data[488..496].fill(0xff);
                self.write_sector(lba, &data)?;
            }
        }
        self.hints_invalid = true;
        Ok(())
    }
    fn chain(&self, first: u32, max: usize) -> Result<Vec<u32>> {
        self.check()?;
        let mut out = Vec::new();
        if first == 0 {
            return Ok(out);
        }
        let mut cluster = first;
        loop {
            if !self.valid(cluster) || out.len() >= max || out.contains(&cluster) {
                return Err("invalid or cyclic FAT chain");
            }
            out.push(cluster);
            let next = self.get(cluster)?;
            if self.is_eof(next) {
                return Ok(out);
            }
            cluster = next;
        }
    }
    fn sync_hints(&mut self) -> Result<()> {
        for lba in [self.fsinfo, self.backup_fsinfo].into_iter().flatten() {
            let mut data = sector(lba)?;
            if u32le(&data, 0) == 0x41615252
                && u32le(&data, 484) == 0x61417272
                && u32le(&data, 508) == 0xaa550000
            {
                data[488..492].copy_from_slice(&self.free_count.to_le_bytes());
                data[492..496].copy_from_slice(&self.hint.to_le_bytes());
                self.write_sector(lba, &data)?;
            }
        }
        self.hints_invalid = false;
        Ok(())
    }
    fn allocate(&mut self, count: usize) -> Result<Vec<u32>> {
        self.check()?;
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut selected = Vec::new();
        for offset in 0..self.clusters {
            let cluster = 2 + (self.hint - 2 + offset) % self.clusters;
            if self.get(cluster)? == 0 {
                selected.push(cluster);
                if selected.len() == count {
                    break;
                }
            }
        }
        if selected.len() != count {
            return Err("disk full");
        }
        self.invalidate_hints()?;
        for (index, &cluster) in selected.iter().enumerate() {
            self.set(
                cluster,
                selected.get(index + 1).copied().unwrap_or(self.eof()),
            )?;
        }
        self.hint = 2 + (selected[selected.len() - 1] - 1) % self.clusters;
        self.free_count -= selected.len() as u32;
        self.sync_hints()?;
        Ok(selected)
    }
    fn free(&mut self, first: u32) -> Result<()> {
        let chain = self.chain(first, MAX_FILE / self.bytes() + 1)?;
        self.invalidate_hints()?;
        for cluster in chain {
            self.set(cluster, 0)?;
            self.hint = self.hint.min(cluster);
            self.free_count += 1;
        }
        self.sync_hints()?;
        Ok(())
    }
    fn read_cluster(&self, cluster: u32) -> Result<Vec<u8>> {
        if !self.valid(cluster) {
            return Err("invalid FAT cluster");
        }
        let mut data = vec![0; self.bytes()];
        for (index, chunk) in data.chunks_mut(SECTOR).enumerate() {
            if !blk::read_sectors(self.lba(cluster) + index as u64, chunk) {
                return Err("disk read failed");
            }
        }
        Ok(data)
    }
    fn write_cluster(&mut self, cluster: u32, data: &[u8]) -> Result<()> {
        if !self.valid(cluster) || data.len() != self.bytes() {
            return Err("invalid cluster write");
        }
        for (index, chunk) in data.chunks(SECTOR).enumerate() {
            self.write_sector(self.lba(cluster) + index as u64, chunk)?;
        }
        Ok(())
    }
    fn load_dir(&self, dir: Dir) -> Result<(Vec<u32>, Vec<u8>)> {
        self.check()?;
        if dir.root && self.kind != Kind::Fat32 {
            let mut data = vec![0; ((self.root_entries * 32 + SECTOR - 1) / SECTOR) * SECTOR];
            for (i, chunk) in data.chunks_mut(SECTOR).enumerate() {
                if !blk::read_sectors(self.root_lba + i as u64, chunk) {
                    return Err("directory read failed");
                }
            }
            data.truncate(self.root_entries * 32);
            return Ok((Vec::new(), data));
        }
        let chain = self.chain(dir.cluster, MAX_METADATA / self.bytes())?;
        if chain.is_empty() {
            return Err("directory has no cluster");
        }
        let mut data = Vec::with_capacity(chain.len() * self.bytes());
        for &cluster in &chain {
            data.extend_from_slice(&self.read_cluster(cluster)?);
        }
        Ok((chain, data))
    }
    fn store_dir(
        &mut self,
        dir: Dir,
        chain: &[u32],
        data: &[u8],
        start: usize,
        len: usize,
    ) -> Result<()> {
        if start + len > data.len() {
            return Err("invalid directory update");
        }
        for index in start / SECTOR..(start + len + SECTOR - 1) / SECTOR {
            let mut bytes = [0; SECTOR];
            let begin = index * SECTOR;
            let take = SECTOR.min(data.len() - begin);
            if take < SECTOR {
                bytes = sector(self.root_lba + index as u64)?;
            }
            bytes[..take].copy_from_slice(&data[begin..begin + take]);
            let lba = if dir.root && self.kind != Kind::Fat32 {
                self.root_lba + index as u64
            } else {
                self.lba(chain[begin / self.bytes()]) + (begin % self.bytes() / SECTOR) as u64
            };
            self.write_sector(lba, &bytes)?;
        }
        Ok(())
    }
    fn lookup(&self, path: &str) -> Result<(Dir, Record)> {
        let components = components(path);
        let mut dir = self.root();
        for (index, part) in components.iter().enumerate() {
            let (_, data) = self.load_dir(dir)?;
            let record = records(&data, self.kind)
                .into_iter()
                .find(|r| same(&r.name, part) || same(&r.alias, part))
                .ok_or("file not found")?;
            if index + 1 == components.len() {
                return Ok((dir, record));
            }
            if !record.is_dir() {
                return Err("not a directory");
            }
            dir = Dir {
                cluster: record.cluster,
                root: false,
            };
        }
        Err("cannot operate on the root directory")
    }
    fn directory(&self, path: &str) -> Result<Dir> {
        if components(path).is_empty() {
            return Ok(self.root());
        }
        let (_, record) = self.lookup(path)?;
        if !record.is_dir() {
            return Err("not a directory");
        }
        Ok(Dir {
            cluster: record.cluster,
            root: false,
        })
    }
    fn parent<'a>(&self, path: &'a str) -> Result<(Dir, &'a str)> {
        let mut parts = components(path);
        let name = parts.pop().ok_or("missing file name")?;
        let mut dir = self.root();
        for part in parts {
            let (_, data) = self.load_dir(dir)?;
            let r = records(&data, self.kind)
                .into_iter()
                .find(|r| same(&r.name, part) || same(&r.alias, part))
                .ok_or("no such directory")?;
            if !r.is_dir() {
                return Err("not a directory");
            }
            dir = Dir {
                cluster: r.cluster,
                root: false,
            };
        }
        Ok((dir, name))
    }
    fn data(&self, record: &Record) -> Result<Vec<u8>> {
        if record.is_dir() {
            return Err("is a directory");
        }
        if record.size as usize > MAX_FILE {
            return Err("file exceeds 16 MiB driver limit");
        }
        let needed = (record.size as usize + self.bytes() - 1) / self.bytes();
        let chain = self.chain(record.cluster, MAX_FILE / self.bytes() + 1)?;
        if chain.len() < needed {
            return Err("file chain shorter than file size");
        }
        let mut data = Vec::with_capacity(record.size as usize);
        for &cluster in chain.iter().take(needed) {
            let chunk = self.read_cluster(cluster)?;
            let take = chunk.len().min(record.size as usize - data.len());
            data.extend_from_slice(&chunk[..take]);
        }
        Ok(data)
    }
    fn write_data(&mut self, data: &[u8]) -> Result<u32> {
        if data.len() > MAX_FILE {
            return Err("file exceeds 16 MiB driver limit");
        }
        let clusters = self.allocate((data.len() + self.bytes() - 1) / self.bytes())?;
        let mut buffer = vec![0; self.bytes()];
        for (index, &cluster) in clusters.iter().enumerate() {
            buffer.fill(0);
            let start = index * buffer.len();
            let take = buffer.len().min(data.len() - start);
            buffer[..take].copy_from_slice(&data[start..start + take]);
            self.write_cluster(cluster, &buffer)?;
        }
        Ok(clusters.first().copied().unwrap_or(0))
    }
    fn insert(&mut self, dir: Dir, name: &str, first: u32, size: u32, attr: u8) -> Result<()> {
        validate_name(name)?;
        let (mut chain, mut data) = self.load_dir(dir)?;
        let existing = records(&data, self.kind);
        if existing
            .iter()
            .any(|r| same(&r.name, name) || same(&r.alias, name))
        {
            return Err("already exists");
        }
        let short = short_name(name, &existing)?;
        let entries = entry_set(name, short, first, size, attr, self.kind);
        let mut start;
        loop {
            start = free_slots(&data, entries.len() / 32);
            if start.is_some() {
                break;
            }
            if dir.root && self.kind != Kind::Fat32 {
                return Err("FAT12/16 root directory full");
            }
            if data.len() + self.bytes() > MAX_METADATA {
                return Err("directory too large");
            }
            let new = self.allocate(1)?[0];
            self.write_cluster(new, &vec![0; self.bytes()])?;
            self.set(*chain.last().ok_or("empty directory chain")?, new)?;
            chain.push(new);
            data.resize(data.len() + self.bytes(), 0);
        }
        let start = start.unwrap();
        let end = start + entries.len();
        let consumed_end = data[start..end].chunks_exact(32).any(|e| e[0] == 0);
        if consumed_end && end < data.len() {
            // Anything after an old end marker is unspecified, not
            // necessarily zero. Keep it hidden behind a new end marker.
            data[end] = 0;
            self.store_dir(dir, &chain, &data, end, 32)?;
        }
        // Publish the short entry last; incomplete LFN prefixes are ignored.
        if entries.len() > 32 {
            data[start..start + entries.len() - 32].copy_from_slice(&entries[..entries.len() - 32]);
            self.store_dir(dir, &chain, &data, start, entries.len() - 32)?;
        }
        data[start + entries.len() - 32..start + entries.len()]
            .copy_from_slice(&entries[entries.len() - 32..]);
        self.store_dir(dir, &chain, &data, start + entries.len() - 32, 32)
    }
    fn update(&mut self, dir: Dir, record: &Record, first: u32, size: u32) -> Result<()> {
        let (chain, mut data) = self.load_dir(dir)?;
        if data[record.offset..record.offset + 11] != record.short {
            return Err("stale directory entry");
        }
        put_cluster(
            &mut data[record.offset..record.offset + 32],
            first,
            self.kind,
        );
        data[record.offset + 28..record.offset + 32].copy_from_slice(&size.to_le_bytes());
        self.store_dir(dir, &chain, &data, record.offset, 32)
    }
}

fn same(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_uppercase)
        .eq(b.chars().flat_map(char::to_uppercase))
}
fn components(path: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts
}
fn checksum(short: &[u8; 11]) -> u8 {
    short
        .iter()
        .fold(0u8, |sum, &b| sum.rotate_right(1).wrapping_add(b))
}
// Default DOS OEM mapping; VFAT long names use UTF-16 independently.
const CP437: [char; 128] = [
    '\u{c7}', '\u{fc}', '\u{e9}', '\u{e2}', '\u{e4}', '\u{e0}', '\u{e5}', '\u{e7}', '\u{ea}',
    '\u{eb}', '\u{e8}', '\u{ef}', '\u{ee}', '\u{ec}', '\u{c4}', '\u{c5}', '\u{c9}', '\u{e6}',
    '\u{c6}', '\u{f4}', '\u{f6}', '\u{f2}', '\u{fb}', '\u{f9}', '\u{ff}', '\u{d6}', '\u{dc}',
    '\u{a2}', '\u{a3}', '\u{a5}', '\u{20a7}', '\u{192}', '\u{e1}', '\u{ed}', '\u{f3}', '\u{fa}',
    '\u{f1}', '\u{d1}', '\u{aa}', '\u{ba}', '\u{bf}', '\u{2310}', '\u{ac}', '\u{bd}', '\u{bc}',
    '\u{a1}', '\u{ab}', '\u{bb}', '\u{2591}', '\u{2592}', '\u{2593}', '\u{2502}', '\u{2524}',
    '\u{2561}', '\u{2562}', '\u{2556}', '\u{2555}', '\u{2563}', '\u{2551}', '\u{2557}', '\u{255d}',
    '\u{255c}', '\u{255b}', '\u{2510}', '\u{2514}', '\u{2534}', '\u{252c}', '\u{251c}', '\u{2500}',
    '\u{253c}', '\u{255e}', '\u{255f}', '\u{255a}', '\u{2554}', '\u{2569}', '\u{2566}', '\u{2560}',
    '\u{2550}', '\u{256c}', '\u{2567}', '\u{2568}', '\u{2564}', '\u{2565}', '\u{2559}', '\u{2558}',
    '\u{2552}', '\u{2553}', '\u{256b}', '\u{256a}', '\u{2518}', '\u{250c}', '\u{2588}', '\u{2584}',
    '\u{258c}', '\u{2590}', '\u{2580}', '\u{3b1}', '\u{df}', '\u{393}', '\u{3c0}', '\u{3a3}',
    '\u{3c3}', '\u{b5}', '\u{3c4}', '\u{3a6}', '\u{398}', '\u{3a9}', '\u{3b4}', '\u{221e}',
    '\u{3c6}', '\u{3b5}', '\u{2229}', '\u{2261}', '\u{b1}', '\u{2265}', '\u{2264}', '\u{2320}',
    '\u{2321}', '\u{f7}', '\u{2248}', '\u{b0}', '\u{2219}', '\u{b7}', '\u{221a}', '\u{207f}',
    '\u{b2}', '\u{25a0}', '\u{a0}',
];
fn alias(short: &[u8; 11], case: u8) -> String {
    let mut name = String::new();
    for (part_index, (part, lower)) in [(&short[..8], case & 8 != 0), (&short[8..], case & 16 != 0)]
        .into_iter()
        .enumerate()
    {
        let len = part.iter().rposition(|&c| c != b' ').map_or(0, |n| n + 1);
        if part_index == 1 && len > 0 {
            name.push('.');
        }
        for (index, &byte) in part[..len].iter().enumerate() {
            let byte = if part_index == 0 && index == 0 && byte == 5 {
                0xe5
            } else {
                byte
            };
            let c = if byte < 128 {
                byte as char
            } else {
                CP437[(byte - 128) as usize]
            };
            if lower {
                name.extend(c.to_lowercase());
            } else {
                name.push(c);
            }
        }
    }
    name
}

fn records(data: &[u8], kind: Kind) -> Vec<Record> {
    let mut out = Vec::new();
    let mut utf16 = [0xffffu16; 260];
    let mut remaining = 0usize;
    let mut total = 0usize;
    let mut sum = 0u8;
    let mut start = 0usize;
    for (index, e) in data.chunks_exact(32).enumerate() {
        if e[0] == 0 {
            break;
        }
        if e[0] == 0xe5 {
            remaining = 0;
            total = 0;
            continue;
        }
        if e[11] == 0x0f {
            let ordinal = (e[0] & 0x1f) as usize;
            if e[0] & 0x40 != 0 {
                total = 0;
                remaining = 0;
                if ordinal > 0 && ordinal <= 20 && e[0] & 0xa0 == 0 {
                    utf16.fill(0xffff);
                    remaining = ordinal;
                    total = ordinal;
                    sum = e[13];
                    start = index * 32;
                }
            }
            if remaining == 0
                || ordinal != remaining
                || e[13] != sum
                || e[12] != 0
                || u16le(e, 26) != 0
                || e[0] & 0xa0 != 0
            {
                remaining = 0;
                total = 0;
                continue;
            }
            for (i, &pos) in LFN_POS.iter().enumerate() {
                utf16[(ordinal - 1) * 13 + i] = u16le(e, pos);
            }
            remaining -= 1;
            continue;
        }
        let short: [u8; 11] = e[..11].try_into().unwrap();
        let alias = alias(&short, e[12]);
        let mut name = alias.clone();
        let mut first_offset = index * 32;
        if total > 0 && remaining == 0 && sum == checksum(&short) {
            let slice = &utf16[..total * 13];
            let len = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
            if len > 0
                && len <= 255
                && !slice[..len].contains(&0xffff)
                && slice[len + usize::from(len < slice.len())..]
                    .iter()
                    .all(|&c| c == 0xffff)
            {
                if let Ok(long) = String::from_utf16(&slice[..len]) {
                    if validate_name(&long).is_ok() {
                        name = long;
                        first_offset = start;
                    }
                }
            }
        }
        remaining = 0;
        total = 0;
        if e[11] & 8 != 0 || alias == "." || alias == ".." {
            continue;
        }
        let cluster = u16le(e, 26) as u32
            | if kind == Kind::Fat32 {
                (u16le(e, 20) as u32 & 0xfff) << 16
            } else {
                0
            };
        out.push(Record {
            name,
            alias,
            short,
            attr: e[11],
            cluster,
            size: u32le(e, 28),
            offset: index * 32,
            start: first_offset,
        });
    }
    out
}
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with([' ', '.'])
        || name.encode_utf16().count() > 255
        || name.chars().any(|c| c < ' ' || "\"*/:<>?\\|".contains(c))
    {
        return Err("invalid FAT file name");
    }
    Ok(())
}
fn short_char(c: char) -> u8 {
    if c.is_ascii_alphanumeric() || "$%'-_@~`!(){}^#&".contains(c) {
        c.to_ascii_uppercase() as u8
    } else {
        b'_'
    }
}
fn short_name(name: &str, existing: &[Record]) -> Result<[u8; 11]> {
    let (base, ext) = name
        .rsplit_once('.')
        .filter(|(base, _)| !base.is_empty())
        .unwrap_or((name, ""));
    let mut direct = [b' '; 11];
    let simple = !base.is_empty()
        && base.len() <= 8
        && ext.len() <= 3
        && base
            .chars()
            .chain(ext.chars())
            .all(|c| c.is_ascii() && short_char(c) == c.to_ascii_uppercase() as u8);
    if simple {
        for (i, c) in base.chars().enumerate() {
            direct[i] = short_char(c);
        }
        for (i, c) in ext.chars().enumerate() {
            direct[8 + i] = short_char(c);
        }
        if !existing.iter().any(|r| r.short == direct) {
            return Ok(direct);
        }
    }
    let clean: Vec<u8> = base
        .chars()
        .filter(|&c| c != ' ' && c != '.')
        .map(short_char)
        .collect();
    for number in 1..1_000_000u32 {
        let mut digits = [0u8; 6];
        let mut n = number;
        let mut len = 0;
        while n > 0 {
            digits[5 - len] = b'0' + (n % 10) as u8;
            n /= 10;
            len += 1;
        }
        let prefix = (7 - len).min(clean.len());
        let mut short = [b' '; 11];
        short[..prefix].copy_from_slice(&clean[..prefix]);
        short[prefix] = b'~';
        short[prefix + 1..prefix + 1 + len].copy_from_slice(&digits[6 - len..]);
        for (i, c) in ext.chars().take(3).enumerate() {
            short[8 + i] = short_char(c);
        }
        if !existing.iter().any(|r| r.short == short) {
            return Ok(short);
        }
    }
    Err("short-name aliases exhausted")
}
fn put_cluster(entry: &mut [u8], first: u32, kind: Kind) {
    entry[26..28].copy_from_slice(&(first as u16).to_le_bytes());
    entry[20..22].copy_from_slice(
        &(if kind == Kind::Fat32 {
            (first >> 16) as u16
        } else {
            0
        })
        .to_le_bytes(),
    );
}
fn entry_set(name: &str, short: [u8; 11], first: u32, size: u32, attr: u8, kind: Kind) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let count = if name == alias(&short, 0) {
        0
    } else {
        (units.len() + 12) / 13
    };
    let mut out = vec![0; (count + 1) * 32];
    for index in 0..count {
        let ordinal = count - index;
        let e = &mut out[index * 32..index * 32 + 32];
        e[0] = ordinal as u8 | if index == 0 { 0x40 } else { 0 };
        e[11] = 0x0f;
        e[13] = checksum(&short);
        for (i, &pos) in LFN_POS.iter().enumerate() {
            let offset = (ordinal - 1) * 13 + i;
            let value = units
                .get(offset)
                .copied()
                .unwrap_or(if offset == units.len() { 0 } else { 0xffff });
            e[pos..pos + 2].copy_from_slice(&value.to_le_bytes());
        }
    }
    let e = &mut out[count * 32..];
    e[..11].copy_from_slice(&short);
    e[11] = attr;
    // Valid deterministic date (1980-01-01), until an RTC service exists.
    for offset in [16, 18, 24] {
        e[offset..offset + 2].copy_from_slice(&0x21u16.to_le_bytes());
    }
    put_cluster(e, first, kind);
    e[28..32].copy_from_slice(&size.to_le_bytes());
    out
}
fn free_slots(data: &[u8], needed: usize) -> Option<usize> {
    let mut run = 0;
    let mut end = false;
    for (index, entry) in data.chunks_exact(32).enumerate() {
        end |= entry[0] == 0;
        if end || entry[0] == 0xe5 {
            run += 1;
            if run == needed {
                return Some((index + 1 - needed) * 32);
            }
        } else {
            run = 0;
        }
    }
    None
}

fn probe(start: u64, length: u64) -> Result<Volume> {
    let b = sector(start)?;
    if b[510..512] != [0x55, 0xaa]
        || !matches!(b[0], 0xeb | 0xe9)
        || u16le(&b, 11) != 512
        || &b[3..11] == b"EXFAT   "
    {
        return Err("not a supported FAT boot sector");
    }
    let spc = b[13] as u32;
    let reserved = u16le(&b, 14) as u32;
    let fats = b[16];
    let roots = u16le(&b, 17) as usize;
    let total = if u16le(&b, 19) != 0 {
        u16le(&b, 19) as u32
    } else {
        u32le(&b, 32)
    };
    let fat_sectors = if u16le(&b, 22) != 0 {
        u16le(&b, 22) as u32
    } else {
        u32le(&b, 36)
    };
    if spc == 0
        || !spc.is_power_of_two()
        || spc > 128
        || reserved == 0
        || !(1..=2).contains(&fats)
        || fat_sectors == 0
        || total as u64 > length
    {
        return Err("invalid FAT geometry");
    }
    let root_sectors = (roots as u64 * 32 + 511) / 512;
    let overhead = reserved as u64 + fats as u64 * fat_sectors as u64 + root_sectors;
    if total as u64 <= overhead {
        return Err("invalid FAT data region");
    }
    let clusters = ((total as u64 - overhead) / spc as u64) as u32;
    let kind = if clusters < 4085 {
        Kind::Fat12
    } else if clusters < 65525 {
        Kind::Fat16
    } else {
        Kind::Fat32
    };
    if clusters == 0 || clusters > 0x0fffffee {
        return Err("invalid FAT cluster count");
    }
    let fat_bytes = fat_sectors as u64 * 512;
    let needed = match kind {
        Kind::Fat12 => ((clusters as u64 + 2) * 3 + 1) / 2,
        Kind::Fat16 => (clusters as u64 + 2) * 2,
        Kind::Fat32 => (clusters as u64 + 2) * 4,
    };
    if fat_bytes < needed || fat_bytes > MAX_METADATA as u64 {
        return Err("unsupported FAT table size");
    }
    let (root_cluster, active, mirrored, fsinfo, backup_fsinfo) = if kind == Kind::Fat32 {
        if roots != 0 || u16le(&b, 22) != 0 || u16le(&b, 42) != 0 {
            return Err("invalid FAT32 BPB");
        }
        let root = u32le(&b, 44) & 0x0fffffff;
        let flags = u16le(&b, 40);
        let mirrored = flags & 0x80 == 0;
        let active = if mirrored { 0 } else { (flags & 0xf) as u8 };
        if root < 2 || root >= clusters + 2 || active >= fats {
            return Err("invalid FAT32 root or active FAT");
        }
        let info = u16le(&b, 48) as u32;
        let backup = u16le(&b, 50) as u32;
        let fsinfo = if info != 0 && info < reserved {
            Some(start + info as u64)
        } else {
            None
        };
        let backup_fsinfo = if fsinfo.is_some() && backup > 0 && backup + info < reserved {
            Some(start + (backup + info) as u64)
        } else {
            None
        };
        (root, active, mirrored, fsinfo, backup_fsinfo)
    } else {
        if roots == 0 || u16le(&b, 22) == 0 {
            return Err("invalid FAT12/16 BPB");
        }
        (0, 0, true, None, None)
    };
    let fat_lba = start + reserved as u64;
    let mut fat = vec![0; fat_bytes as usize];
    for (index, chunk) in fat.chunks_mut(SECTOR).enumerate() {
        if !blk::read_sectors(
            fat_lba + active as u64 * fat_sectors as u64 + index as u64,
            chunk,
        ) {
            return Err("FAT read failed");
        }
    }
    let root_lba = fat_lba + fats as u64 * fat_sectors as u64;
    let mut volume = Volume {
        kind,
        fat_lba,
        fat_sectors,
        fats,
        active,
        mirrored,
        data_lba: root_lba + root_sectors,
        spc,
        clusters,
        root_lba,
        root_entries: roots,
        root_cluster,
        fat,
        hint: 2,
        fsinfo,
        backup_fsinfo,
        hints_invalid: false,
        free_count: 0,
        failed: false,
    };
    for cluster in 2..clusters + 2 {
        if volume.get(cluster)? == 0 {
            volume.free_count += 1;
        }
    }
    Ok(volume)
}
fn discover() -> Result<Volume> {
    let capacity = blk::capacity_sectors();
    if capacity == 0 {
        return Err("no block device");
    }
    if let Ok(volume) = probe(0, capacity) {
        return Ok(volume);
    }
    let mbr = sector(0)?;
    if mbr[510..512] != [0x55, 0xaa] {
        return Err("no FAT filesystem");
    }
    let protective = mbr[446..510].chunks_exact(16).any(|p| p[4] == 0xee);
    if protective {
        let mut header = sector(1)?;
        let header_len = u32le(&header, 12) as usize;
        if &header[..8] != b"EFI PART"
            || !(92..=512).contains(&header_len)
            || u64le(&header, 24) != 1
        {
            return Err("invalid GPT header");
        }
        let expected = u32le(&header, 16);
        header[16..20].fill(0);
        if crc32(&header[..header_len]) != expected {
            return Err("GPT header checksum mismatch");
        }
        let first_usable = u64le(&header, 40);
        let last_usable = u64le(&header, 48);
        let table_lba = u64le(&header, 72);
        let count = u32le(&header, 80) as usize;
        let size = u32le(&header, 84) as usize;
        if count == 0
            || count > 256
            || !(128..=4096).contains(&size)
            || size % 8 != 0
            || table_lba < 2
            || table_lba >= capacity
            || first_usable > last_usable
            || last_usable >= capacity
        {
            return Err("unsupported GPT layout");
        }
        let bytes = count * size;
        let sectors = (bytes + 511) / 512;
        if sectors as u64 > capacity - table_lba || table_lba + sectors as u64 > first_usable {
            return Err("invalid GPT table bounds");
        }
        let mut table = vec![0; sectors * 512];
        for (i, chunk) in table.chunks_mut(512).enumerate() {
            if !blk::read_sectors(table_lba + i as u64, chunk) {
                return Err("GPT read failed");
            }
        }
        if crc32(&table[..bytes]) != u32le(&header, 88) {
            return Err("GPT partition checksum mismatch");
        }
        for part in table[..bytes].chunks_exact(size) {
            if part[..16].iter().all(|&b| b == 0) {
                continue;
            }
            let first = u64le(part, 32);
            let last = u64le(part, 40);
            if first < first_usable || last > last_usable || first > last {
                return Err("invalid GPT partition bounds");
            }
            if let Ok(volume) = probe(first, last - first + 1) {
                return Ok(volume);
            }
        }
    } else {
        for part in mbr[446..510].chunks_exact(16) {
            if part[4] == 0 {
                continue;
            }
            let first = u32le(part, 8) as u64;
            let length = u32le(part, 12) as u64;
            if first == 0 || first >= capacity || length > capacity - first {
                continue;
            }
            if let Ok(volume) = probe(first, length) {
                return Ok(volume);
            }
        }
    }
    Err("no supported FAT volume found")
}

pub fn mount() -> bool {
    *VOLUME.lock() = None;
    match discover() {
        Ok(volume) => {
            let name = match volume.kind {
                Kind::Fat12 => "FAT12",
                Kind::Fat16 => "FAT16",
                Kind::Fat32 => "FAT32",
            };
            crate::serial::write_str("[FS] ");
            crate::serial::write_str(name);
            crate::serial::write_line(" mounted (VFAT long filenames)");
            *VOLUME.lock() = Some(volume);
            true
        }
        Err(_) => false,
    }
}
pub fn mounted() -> bool {
    VOLUME.lock().is_some()
}
pub fn read_dir(path: &str) -> Option<Vec<(String, bool, u64)>> {
    let guard = VOLUME.lock();
    let vol = guard.as_ref()?;
    let dir = vol.directory(path).ok()?;
    let (_, data) = vol.load_dir(dir).ok()?;
    Some(
        records(&data, vol.kind)
            .into_iter()
            .map(|r| {
                let is_dir = r.is_dir();
                (r.name, is_dir, r.size as u64)
            })
            .collect(),
    )
}
pub fn read_file(path: &str) -> Option<Vec<u8>> {
    let guard = VOLUME.lock();
    let vol = guard.as_ref()?;
    let (_, record) = vol.lookup(path).ok()?;
    vol.data(&record).ok()
}
pub fn metadata(path: &str) -> Option<(bool, u64)> {
    let guard = VOLUME.lock();
    let vol = guard.as_ref()?;
    vol.check().ok()?;
    let (_, record) = vol.lookup(path).ok()?;
    Some((record.is_dir(), record.size as u64))
}
pub fn directory_id(path: &str) -> Option<u64> {
    let guard = VOLUME.lock();
    let vol = guard.as_ref()?;
    vol.check().ok()?;
    Some(vol.directory(path).ok()?.cluster as u64)
}
fn create(vol: &mut Volume, path: &str, data: &[u8]) -> Result<()> {
    vol.check()?;
    let (parent, name) = vol.parent(path)?;
    validate_name(name)?;
    let (_, dir) = vol.load_dir(parent)?;
    if records(&dir, vol.kind)
        .iter()
        .any(|r| same(&r.name, name) || same(&r.alias, name))
    {
        return Err("already exists");
    }
    let first = vol.write_data(data)?;
    if let Err(error) = vol.insert(parent, name, first, data.len() as u32, 0x20) {
        if !vol.failed {
            let _ = vol.free(first);
        }
        return Err(error);
    }
    Ok(())
}
pub fn create_file(path: &str, data: &[u8]) -> Result<()> {
    let mut guard = VOLUME.lock();
    create(guard.as_mut().ok_or("not mounted")?, path, data)
}
fn overwrite(vol: &mut Volume, path: &str, data: &[u8]) -> Result<()> {
    vol.check()?;
    // Resolve the parent separately so a missing file can be created,
    // whereas directory read errors can never masquerade as "not found".
    let (parent, name) = vol.parent(path)?;
    validate_name(name)?;
    let (_, dir) = vol.load_dir(parent)?;
    let record = records(&dir, vol.kind)
        .into_iter()
        .find(|r| same(&r.name, name) || same(&r.alias, name));
    match record {
        None => create(vol, path, data),
        Some(record) => {
            if record.is_dir() {
                return Err("is a directory");
            }
            if record.attr & 1 != 0 {
                return Err("file is read-only");
            }
            let first = vol.write_data(data)?;
            if let Err(error) = vol.update(parent, &record, first, data.len() as u32) {
                if !vol.failed {
                    let _ = vol.free(first);
                }
                return Err(error);
            }
            vol.free(record.cluster)
        }
    }
}
pub fn write_file(path: &str, data: &[u8]) -> Result<()> {
    let mut guard = VOLUME.lock();
    overwrite(guard.as_mut().ok_or("not mounted")?, path, data)
}
pub fn append_file(path: &str, data: &[u8]) -> Result<()> {
    let mut guard = VOLUME.lock();
    let vol = guard.as_mut().ok_or("not mounted")?;
    let (parent, name) = vol.parent(path)?;
    let (_, dir) = vol.load_dir(parent)?;
    match records(&dir, vol.kind)
        .into_iter()
        .find(|r| same(&r.name, name) || same(&r.alias, name))
    {
        None => create(vol, path, data),
        Some(record) => {
            let mut old = vol.data(&record)?;
            if old
                .len()
                .checked_add(data.len())
                .filter(|&n| n <= MAX_FILE)
                .is_none()
            {
                return Err("file exceeds 16 MiB driver limit");
            }
            old.extend_from_slice(data);
            overwrite(vol, path, &old)
        }
    }
}
pub fn mkdir(path: &str) -> Result<()> {
    let mut guard = VOLUME.lock();
    let vol = guard.as_mut().ok_or("not mounted")?;
    vol.check()?;
    let (parent, name) = vol.parent(path)?;
    validate_name(name)?;
    let (_, dir) = vol.load_dir(parent)?;
    if records(&dir, vol.kind)
        .iter()
        .any(|r| same(&r.name, name) || same(&r.alias, name))
    {
        return Err("already exists");
    }
    let cluster = vol.allocate(1)?[0];
    let mut data = vec![0; vol.bytes()];
    let dot = entry_set(".", *b".          ", cluster, 0, 0x10, vol.kind);
    let dotdot = entry_set(
        "..",
        *b"..         ",
        if parent.root { 0 } else { parent.cluster },
        0,
        0x10,
        vol.kind,
    );
    data[..32].copy_from_slice(&dot[dot.len() - 32..]);
    data[32..64].copy_from_slice(&dotdot[dotdot.len() - 32..]);
    vol.write_cluster(cluster, &data)?;
    if let Err(error) = vol.insert(parent, name, cluster, 0, 0x10) {
        if !vol.failed {
            let _ = vol.free(cluster);
        }
        return Err(error);
    }
    Ok(())
}
pub fn delete(path: &str) -> Result<()> {
    let mut guard = VOLUME.lock();
    let vol = guard.as_mut().ok_or("not mounted")?;
    vol.check()?;
    let (parent, record) = vol.lookup(path)?;
    if record.attr & 1 != 0 {
        return Err("file is read-only");
    }
    if record.is_dir() {
        let (_, data) = vol.load_dir(Dir {
            cluster: record.cluster,
            root: false,
        })?;
        if !records(&data, vol.kind).is_empty() {
            return Err("directory not empty");
        }
    }
    // Validate the chain before unlinking; stop on corruption without
    // making a damaged volume harder to recover.
    vol.chain(record.cluster, MAX_FILE / vol.bytes() + 1)?;
    let (chain, mut dir) = vol.load_dir(parent)?;
    for offset in (record.start..=record.offset).step_by(32) {
        dir[offset] = 0xe5;
    }
    vol.store_dir(
        parent,
        &chain,
        &dir,
        record.start,
        record.offset + 32 - record.start,
    )?;
    vol.free(record.cluster)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_names_round_trip_and_invalid_chains_fall_back() {
        for name in [
            "Thirteen chars",
            "café 😀 and spaces.txt",
            "abcdefghijklm",
            &"a".repeat(255),
        ] {
            let raw = entry_set(name, *b"LONGNA~1TXT", 2, 7, 0x20, Kind::Fat16);
            let r = records(&raw, Kind::Fat16);
            assert_eq!(r.len(), 1);
            assert_eq!(r[0].name, name);
            assert_eq!(r[0].cluster, 2);
            assert_eq!(r[0].size, 7);
            let mut bad = raw.clone();
            bad[13] ^= 1;
            assert_eq!(records(&bad, Kind::Fat16)[0].name, "LONGNA~1.TXT");
            bad = raw.clone();
            bad[0] = 0x40 | 21;
            assert_eq!(records(&bad, Kind::Fat16)[0].name, "LONGNA~1.TXT");
        }
    }
    #[test]
    fn unique_short_names_and_unicode_limits() {
        let mut existing = records(
            &entry_set(
                "Long filename.txt",
                *b"LONGFI~1TXT",
                0,
                0,
                0x20,
                Kind::Fat12,
            ),
            Kind::Fat12,
        );
        assert_eq!(
            short_name("Long filename two.txt", &existing).unwrap(),
            *b"LONGFI~2TXT"
        );
        existing.extend(records(
            &entry_set(
                "Long filename two.txt",
                *b"LONGFI~2TXT",
                0,
                0,
                0x20,
                Kind::Fat12,
            ),
            Kind::Fat12,
        ));
        assert_eq!(
            short_name("Long filename three.txt", &existing).unwrap(),
            *b"LONGFI~3TXT"
        );
        assert!(validate_name(&"😀".repeat(127)).is_ok());
        assert!(validate_name(&"😀".repeat(128)).is_err());
        assert!(validate_name("trailing.").is_err());
        assert!(validate_name("trailing ").is_err());
    }
    #[test]
    fn lfn_utf16_and_checksum_validation() {
        let raw = entry_set(
            "Long filename.txt",
            *b"LONGFI~1TXT",
            0,
            0,
            0x20,
            Kind::Fat32,
        );
        let mut bad = raw.clone();
        bad[12] = 1;
        assert_eq!(records(&bad, Kind::Fat32)[0].name, "LONGFI~1.TXT");
        bad = raw.clone();
        bad[26] = 1;
        assert_eq!(records(&bad, Kind::Fat32)[0].name, "LONGFI~1.TXT");
        bad = raw.clone();
        bad[32] = 2; // duplicate ordinal instead of 1
        assert_eq!(records(&bad, Kind::Fat32)[0].name, "LONGFI~1.TXT");
        bad = raw;
        bad[33..35].copy_from_slice(&0xd800u16.to_le_bytes());
        assert_eq!(records(&bad, Kind::Fat32)[0].name, "LONGFI~1.TXT");
    }
}
