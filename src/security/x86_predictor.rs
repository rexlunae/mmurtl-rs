//! Capability-gated predictor control ownership; assembly entry code applies
//! the same base bits before the first compiled call or return.
use super::{CommonCaps,Policy,Vulnerability as V,feature as F};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum Mode {Off,Retpolines,LegacyIbrs,EnhancedIbrs,AutoIbrs}
#[derive(Clone,Copy,Debug)]
pub struct Plan {
    pub mode:Mode,pub clear:u64,pub set:u64,pub ibpb:bool,
    pub bhb_loop:bool,pub ibhf:bool,pub rsb:bool,pub sibling_safe:bool,
}
impl Plan {
    pub fn select(caps:CommonCaps,policy:Policy,all_intel:bool,isolating:bool)->Self {
        let v2=policy.enabled(V::SpectreV2);
        let returns=policy.enabled(V::Retbleed)||policy.enabled(V::Srso);
        let mode=if v2&&caps.features&(F::EIBRS|F::IBRS)==(F::EIBRS|F::IBRS) {Mode::EnhancedIbrs}
            else if v2&&caps.features&F::AUTOIBRS!=0 {Mode::AutoIbrs}
            else if all_intel&&returns&&caps.features&F::IBRS!=0 {Mode::LegacyIbrs}
            else if v2 {Mode::Retpolines} else {Mode::Off};
        let mut clear=0;let mut set=0;
        for (feature,mask,enabled) in [(F::IBRS,1,matches!(mode,Mode::LegacyIbrs|Mode::EnhancedIbrs)),
            (F::STIBP,2,v2),(F::RRSBA_CTRL,1<<6,v2),
            (F::BHI_CTRL,1<<10,policy.enabled(V::Bhi)),
            (F::IPRED_CTRL,1<<4,policy.enabled(V::Its))] {
            if caps.features&feature!=0 {clear|=mask;if enabled {set|=mask;}}
        }
        let history=policy.enabled(V::Bhi)&&caps.not_affected&V::Bhi.bit()==0;
        Self {mode,clear,set,ibpb:(v2||returns)&&caps.features&F::IBPB!=0,
            bhb_loop:history&&all_intel&&caps.features&F::BHI_CTRL==0,
            ibhf:history&&all_intel,rsb:v2||returns,
            sibling_safe:isolating||v2&&caps.features&F::STIBP!=0}
    }
}
