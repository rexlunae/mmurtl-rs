/// Address-authentication algorithms are mutually exclusive. Generic-only
/// GPA/GPI fields do not provide PAC-ret; reserved encodings fail closed.
pub fn pac_signature(isar1:u64,isar2:u64)->Option<u64> {
    let fields=[(isar1>>4)&15,(isar1>>8)&15,(isar2>>12)&15];
    if fields.iter().any(|v|*v>6)||fields.iter().filter(|v|**v!=0).count()!=1 {return None;}
    Some(fields[0]|fields[1]<<4|fields[2]<<8)
}
pub fn canary(value:u64)->u64 {(value&!255).max(256)}
