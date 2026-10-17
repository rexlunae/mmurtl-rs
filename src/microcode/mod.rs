//! Allocation-free vendor microcode container validation and CPU matching.
//! A format checksum is not authentication. The hardware loader separately
//! requires a pinned AMD SHA-256 digest before touching PATCH_LOADER.
#[derive(Clone, Copy, Debug)]
pub struct Patch<'a> { pub bytes: &'a [u8], pub revision: u32, pub data_offset: usize }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error { Truncated, Format, Checksum, Overflow }
fn u32_at(b: &[u8], off: usize) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(b.get(off..off + 4).ok_or(Error::Truncated)?.try_into().unwrap()))
}
fn u16_at(b: &[u8], off: usize) -> Result<u16, Error> {
    Ok(u16::from_le_bytes(b.get(off..off + 2).ok_or(Error::Truncated)?.try_into().unwrap()))
}
fn sum(b: &[u8]) -> Result<u32, Error> {
    if b.len() % 4 != 0 { return Err(Error::Format); }
    Ok(b.chunks_exact(4).fold(0u32, |s, x| s.wrapping_add(u32::from_le_bytes(x.try_into().unwrap()))))
}
fn matches(sig: u32, platforms: u32, cpu: u32, platform: u32) -> bool {
    sig == cpu && (platforms == 0 || platforms & platform != 0)
}
/// Validate the entire concatenation before returning the newest usable patch.
pub fn intel(mut blob: &[u8], cpu: u32, platform: u32, current: u32) -> Result<Option<Patch<'_>>, Error> {
    let mut selected: Option<Patch<'_>> = None;
    while !blob.is_empty() {
        let header = blob.get(..48).ok_or(Error::Truncated)?;
        if u32_at(header, 0)? != 1 || u32_at(header, 20)? != 1 { return Err(Error::Format); }
        let revision = u32_at(header, 4)?;
        let data = match u32_at(header, 28)? { 0 => 2000, n => n as usize };
        let total = match u32_at(header, 32)? { 0 => 2048, n => n as usize };
        let base = 48usize.checked_add(data).ok_or(Error::Overflow)?;
        if base > total || data % 4 != 0 || total % 4 != 0 || total > 16 * 1024 * 1024 { return Err(Error::Format); }
        let patch = blob.get(..total).ok_or(Error::Truncated)?;
        if sum(&patch[..base])? != 0 { return Err(Error::Checksum); }
        let sig = u32_at(header, 12)?;
        let pf = u32_at(header, 24)?;
        let mut applies = matches(sig, pf, cpu, platform);
        let ext = &patch[base..];
        if !ext.is_empty() {
            if ext.len() < 20 || (ext.len() - 20) % 12 != 0 { return Err(Error::Format); }
            let count = u32_at(ext, 0)? as usize;
            if count.checked_mul(12).and_then(|n| n.checked_add(20)) != Some(ext.len()) { return Err(Error::Format); }
            if sum(ext)? != 0 { return Err(Error::Checksum); }
            let original = sig.wrapping_add(pf).wrapping_add(u32_at(header, 16)?);
            for entry in ext[20..].chunks_exact(12) {
                let esig = u32_at(entry, 0)?; let epf = u32_at(entry, 4)?;
                if esig.wrapping_add(epf).wrapping_add(u32_at(entry, 8)?) != original { return Err(Error::Checksum); }
                applies |= matches(esig, epf, cpu, platform);
            }
        }
        if applies && revision > current && revision & (1 << 31) == 0
            && selected.map_or(true, |p| revision > p.revision) {
            selected = Some(Patch { bytes: patch, revision, data_offset: 48 });
        }
        blob = &blob[total..];
    }
    Ok(selected)
}

pub fn family(cpu: u32) -> u32 { let base = (cpu >> 8) & 15; base + if base == 15 { (cpu >> 20) & 255 } else { 0 } }
fn zen_signature(revision: u32) -> u32 {
    ((revision >> 8) & 0xff) | 0xf00 | ((revision >> 4) & 0xf0000) | ((revision >> 4) & 0xff00000)
}
/// AMD concatenated containers: magic, equivalence table, patch sections.
pub fn amd(blob: &[u8], cpu: u32, current: u32) -> Result<Option<Patch<'_>>, Error> {
    amd_eligible(blob, cpu, current, |_| true)
}
pub fn amd_eligible<'a>(mut blob: &'a [u8], cpu: u32, current: u32, mut eligible: impl FnMut(Patch<'a>) -> bool) -> Result<Option<Patch<'a>>, Error> {
    let fam = family(cpu);
    let mut selected: Option<Patch<'_>> = None;
    while !blob.is_empty() {
        if u32_at(blob, 0)? != 0x00414d44 || u32_at(blob, 4)? != 0 { return Err(Error::Format); }
        let table_len = u32_at(blob, 8)? as usize;
        if table_len % 16 != 0 || (fam < 0x17 && table_len < 16) { return Err(Error::Format); }
        let start = 12usize.checked_add(table_len).ok_or(Error::Overflow)?;
        let table = blob.get(12..start).ok_or(Error::Truncated)?;
        let mut equivalent = None;
        for entry in table.chunks_exact(16) {
            if u32_at(entry, 0)? == cpu {
                // Errata-mask/chipset-specific patches are unsupported.
                if u32_at(entry, 4)? != 0 || u32_at(entry, 8)? != 0 || u16_at(entry, 14)? != 0 { return Err(Error::Format); }
                equivalent = Some(u16_at(entry, 12)?);
            }
        }
        blob = &blob[start..];
        while !blob.is_empty() && u32_at(blob, 0)? != 0x00414d44 {
            if u32_at(blob, 0)? != 1 { return Err(Error::Format); }
            let size = u32_at(blob, 4)? as usize;
            if size < 64 || size > 128 * 1024 { return Err(Error::Format); }
            let end = 8usize.checked_add(size).ok_or(Error::Overflow)?;
            let patch = blob.get(8..end).ok_or(Error::Truncated)?;
            let revision = u32_at(patch, 4)?;
            let processor = u16_at(patch, 24)?;
            let applies = if fam >= 0x17 { zen_signature(revision) == cpu && revision & 0xf0000 == 0 }
                else { equivalent == Some(processor) };
            let family_matches = 0xf + (processor as u32 >> 12) == fam;
            let size_ok = match fam { 0x10..=0x12 => size <= 2048, 0x14 => size <= 1824, 0x15.. => true, _ => false };
            if applies && family_matches && size_ok && u32_at(patch, 16)? == 0 && u32_at(patch, 20)? == 0
                && revision > current && selected.map_or(true, |p| revision > p.revision)
                && eligible(Patch { bytes: patch, revision, data_offset: 0 }) {
                selected = Some(Patch { bytes: patch, revision, data_offset: 0 });
            }
            blob = &blob[end..];
        }
    }
    Ok(selected)
}

pub mod amd_digests;

pub mod amd_policy;

/// Pinned vendor-published bytes, in addition to the CPU's signature checks.
pub fn amd_digest_known(revision: u32, size: usize, digest: &[u8; 32]) -> bool {
    amd_digests::AMD_DIGESTS.iter().any(|(rev, len, hash)| *rev == revision && *len == size && hash == digest)
}
