//! Bounded static ELF/PIE validation, independent of mapping and allocation.
const PAGE: u64 = 4096;
pub const MAX_IMAGE: u64 = 64 << 20;
const MAX_RELOCS: u64 = 4096;
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub vaddr: u64, pub memsz: u64, pub offset: u64, pub filesz: u64,
    pub writable: bool, pub executable: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct Region { pub start: u64, pub end: u64 }
pub struct Image {
    pub entry: u64, pub pie: bool, pub alignment: u64, pub segments: heapless::Vec<Segment, 16>,
    pub relro: Option<Region>, reloc_file: usize, reloc_count: usize,
}
pub fn rd16(b: &[u8], o: usize) -> u16 { u16::from_le_bytes(b[o..o+2].try_into().unwrap()) }
pub fn rd32(b: &[u8], o: usize) -> u32 { u32::from_le_bytes(b[o..o+4].try_into().unwrap()) }
pub fn rd64(b: &[u8], o: usize) -> u64 { u64::from_le_bytes(b[o..o+8].try_into().unwrap()) }
fn file_range(segs: &[Segment], va: u64, len: u64) -> Option<usize> {
    segs.iter().find_map(|s| {
        let delta=va.checked_sub(s.vaddr)?;
        (delta<=s.filesz && len<=s.filesz-delta).then(|| (s.offset+delta) as usize)
    })
}
impl Image {
    pub fn relocations<'a>(&'a self, bytes: &'a [u8]) -> impl Iterator<Item=(u64,i64)>+'a {
        (0..self.reloc_count).map(move |i| {
            let at=self.reloc_file+i*24; (rd64(bytes,at),rd64(bytes,at+16) as i64)
        })
    }
    pub fn readonly_page(&self, page: u64) -> bool {
        self.relro.is_some_and(|r| page>=r.start && page+PAGE<=r.end)
    }
}
pub fn parse(bytes: &[u8], machine: u16, user_base: u64, user_limit: u64) -> Result<Image,&'static str> {
    if bytes.len()<64 || &bytes[..4]!=b"\x7fELF" { return Err("not an ELF file"); }
    if bytes[4]!=2 || bytes[5]!=1 || bytes[6]!=1 || rd32(bytes,20)!=1 { return Err("unsupported ELF format/version"); }
    let pie=match rd16(bytes,16) { 2=>false,3=>true,_=>return Err("not an executable or static PIE") };
    if rd16(bytes,18)!=machine { return Err("built for a different architecture"); }
    if rd16(bytes,52)!=64 { return Err("bad ELF header size"); }
    let phoff=usize::try_from(rd64(bytes,32)).map_err(|_|"bad program header table")?;
    let size=rd16(bytes,54) as usize; let count=rd16(bytes,56) as usize;
    if size<56 || count==0 || count>32 || phoff.checked_add(size*count).filter(|end|*end<=bytes.len()).is_none() {
        return Err("bad program header table");
    }
    let mut image=Image { entry:rd64(bytes,24),pie,alignment:PAGE,segments:heapless::Vec::new(),relro:None,reloc_file:0,reloc_count:0 };
    let mut dynamic=None; let mut total=0u64;
    for i in 0..count {
        let at=phoff+i*size; let kind=rd32(bytes,at); let flags=rd32(bytes,at+4);
        let offset=rd64(bytes,at+8); let va=rd64(bytes,at+16); let filesz=rd64(bytes,at+32); let memsz=rd64(bytes,at+40);
        let end=va.checked_add(memsz).ok_or("segment wraps")?;
        if kind==3 { return Err("dynamic interpreter unsupported"); }
        if kind==0x6474e551 && flags&1!=0 { return Err("executable stack unsupported"); }
        if kind==2 {
            if dynamic.is_some() || !pie || filesz==0 || filesz>memsz || filesz%16!=0 || filesz>4096 { return Err("bad dynamic table"); }
            dynamic=Some((va,filesz,offset));
        }
        if kind==0x6474e552 && memsz!=0 {
            if image.relro.is_some() || va%PAGE!=0 || end%PAGE!=0 { return Err("RELRO must cover whole pages"); }
            image.relro=Some(Region { start:va,end });
        }
        if kind!=1 || memsz==0 { continue; }
        let align=rd64(bytes,at+48);
        if align>1 && (!align.is_power_of_two() || va&(align-1)!=offset&(align-1)) { return Err("invalid segment alignment"); }
        if pie && align>65536 { return Err("PIE alignment exceeds 64 KiB limit"); }
        image.alignment=image.alignment.max(align);
        if flags&!7!=0 || flags&4==0 { return Err("unsupported segment permissions"); }
        if filesz>memsz || offset.checked_add(filesz).filter(|end|*end<=bytes.len() as u64).is_none() {
            return Err("segment data past end of file");
        }
        if flags&3==3 { return Err("writable+executable segment (W^X)"); }
        if pie { if end>MAX_IMAGE { return Err("PIE image exceeds size limit"); } }
        else if va<user_base || end>user_limit { return Err("segment outside the user window"); }
        total=total.checked_add(memsz).ok_or("image too large")?;
        if total>MAX_IMAGE { return Err("image exceeds memory limit"); }
        let first=va&!(PAGE-1); let last=end.checked_add(PAGE-1).ok_or("segment wraps")?&!(PAGE-1);
        if image.segments.iter().any(|s| first < ((s.vaddr+s.memsz+PAGE-1)&!(PAGE-1)) && (s.vaddr&!(PAGE-1))<last) {
            return Err("segments share a page");
        }
        image.segments.push(Segment {vaddr:va,memsz,offset,filesz,writable:flags&2!=0,executable:flags&1!=0})
            .map_err(|_|"too many segments")?;
    }
    if !image.segments.iter().any(|s|s.executable && image.entry>=s.vaddr && image.entry<s.vaddr+s.memsz) {
        return Err("entry point not in an executable segment");
    }
    if let Some(r)=image.relro {
        if !image.segments.iter().any(|s|s.writable && r.start>=s.vaddr && r.end<=s.vaddr+s.memsz) {
            return Err("RELRO outside writable image");
        }
    }
    if let Some((va,len,offset))=dynamic {
        let file=file_range(&image.segments,va,len).ok_or("dynamic table outside file-backed image")?;
        if file as u64!=offset { return Err("inconsistent dynamic table offset"); }
        let (mut rela,mut relasz,mut relaent)=(None,None,None); let mut terminated=false;
        for at in (file..file+len as usize).step_by(16) {
            let tag=rd64(bytes,at); let value=rd64(bytes,at+8);
            if tag==0 { terminated=true; break; }
            match tag {
                7=> { if rela.replace(value).is_some() { return Err("duplicate DT_RELA"); } }
                8=> { if relasz.replace(value).is_some() { return Err("duplicate DT_RELASZ"); } }
                9=> { if relaent.replace(value).is_some() { return Err("duplicate DT_RELAENT"); } }
                1|2|17|18|19|20|22|23|35|36|37=>return Err("unsupported dynamic dependency/relocation format"),
                _=>{}
            }
        }
        if !terminated { return Err("unterminated dynamic table"); }
        match (rela,relasz,relaent) {
            (None,None,None)=>{},
            (Some(va),Some(len),Some(24)) if len%24==0 && len/24<=MAX_RELOCS=>{
                image.reloc_file=file_range(&image.segments,va,len).ok_or("relocations outside file-backed image")?;
                image.reloc_count=(len/24) as usize;
                let relative=match machine {62=>8,183=>1027,_=>return Err("unsupported relocation architecture")};
                for i in 0..image.reloc_count {
                    let at=image.reloc_file+i*24; let target=rd64(bytes,at); let info=rd64(bytes,at+8);
                    if info!=relative { return Err("only symbol-free RELATIVE relocations supported"); }
                    if target%8!=0 || !image.segments.iter().any(|s|s.writable && target>=s.vaddr && target.checked_add(8).is_some_and(|end|end<=s.vaddr+s.memsz)) {
                        return Err("relocation target outside writable image");
                    }
                    let addend=rd64(bytes,at+16) as i64;
                    if addend<0 || addend as u64>MAX_IMAGE { return Err("relative addend outside image bounds"); }
                }
            }
            _=>return Err("invalid RELA table description"),
        }
    }
    Ok(image)
}
