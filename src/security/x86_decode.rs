//! Decode captured CPUID words; read ARCH_CAPABILITIES only when gated.
//! Kept separate from privileged instructions for heterogeneous host tests.
use super::{SpecCaps, Vulnerability as V, feature as F};
pub fn decode(mut caps: SpecCaps, read_arch_caps: impl FnOnce() -> u64) -> SpecCaps {
    caps.features = 0; caps.not_affected = 0; caps.raw[4] = 0;
    for (bit, feature) in [(26,F::IBRS | F::IBPB),(27,F::STIBP),(28,F::L1D_FLUSH),
        (29,F::ARCH_CAPS),(31,F::SSBD),(10,F::MD_CLEAR),(20,F::IBT)] {
        if caps.raw[0] & (1 << bit) != 0 { caps.features |= feature; }
    }
    for (word, bit, feature) in [(5,7,F::SMEP),(5,20,F::SMAP),(6,2,F::UMIP),(7,6,F::LASS),
        (1,0,F::PSFD),(1,1,F::IPRED_CTRL),(1,2,F::RRSBA_CTRL),(1,4,F::BHI_CTRL)] {
        if caps.raw[word] & (1 << bit) != 0 { caps.features |= feature; }
    }
    if caps.features & F::ARCH_CAPS != 0 {
        caps.raw[4] = read_arch_caps();
        for (bit, family) in [(0,V::Meltdown),(4,V::Ssb),(5,V::Mds),(8,V::Taa),
            (20,V::Bhi),(26,V::Gds),(27,V::Rfds),(62,V::Its)] {
            if caps.raw[4] & (1 << bit) != 0 { caps.not_affected |= family.bit(); }
        }
        if caps.raw[4] & ((1 << 13)|(1 << 14)|(1 << 15)) == ((1 << 13)|(1 << 14)|(1 << 15)) {
            caps.not_affected |= V::Mmio.bit();
        }
        for (bit, feature) in [(1,F::EIBRS),(12,F::DOITM),(25,F::GDS_CTRL),(28,F::RFDS_CLEAR)] {
            if caps.raw[4] & (1 << bit) != 0 { caps.features |= feature; }
        }
    }
    if caps.vendor == *b"AuthenticAMD" {
        for (bit, feature) in [(12,F::IBPB),(14,F::IBRS),(15,F::STIBP),
            (24,F::SSBD),(25,F::VIRT_SSBD),(30,F::IBPB_RET)] {
            if caps.raw[2] & (1 << bit) != 0 { caps.features |= feature; }
        }
        if caps.raw[2] & (1 << 26) != 0 { caps.not_affected |= V::Ssb.bit(); }
        if caps.raw[3] & (1 << 8) != 0 { caps.features |= F::AUTOIBRS; }
        if caps.raw[3] & (1 << 27) != 0 { caps.features |= F::SBPB; }
        if caps.raw[3] & (1 << 29) != 0 { caps.not_affected |= V::Srso.bit(); }
    }
    caps
}
