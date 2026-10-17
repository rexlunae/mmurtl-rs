use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::security::store_bypass::{Plan, Backend, Architecture};
static PLAN: spin::Once<Plan> = spin::Once::new();
static VIRT_VALID: [AtomicBool; 64] = [const { AtomicBool::new(false) }; 64];
static VIRT_VALUE: [AtomicU64; 64] = [const { AtomicU64::new(0) }; 64];
pub fn plan() -> Plan { *PLAN.get().expect("store-bypass policy not initialized") }
pub fn init() {
    let plan = Plan::select(crate::security::common(), crate::security::policy(), Architecture::Amd64);
    PLAN.call_once(|| plan);
    if plan.supported() && plan.mode != crate::security::store_bypass::Mode::Off {
        crate::security::installed(crate::security::Vulnerability::Ssb, if plan.mode == crate::security::store_bypass::Mode::On { "forced store-bypass disable" } else { "per-task SSBD policy" });
    }
    crate::serial::write_str("[SPEC] spec_store_bypass: ");
    crate::serial::write_str(match plan.mode { crate::security::store_bypass::Mode::Off => "off ", crate::security::store_bypass::Mode::On => "on ", _ => "per-task " });
    crate::serial::write_line(match plan.backend { Backend::SpecCtrl => "SSBD (cached SPEC_CTRL)",
        Backend::VirtualSpecCtrl => "SSBD (AMD VIRT_SPEC_CTRL)", Backend::NotAffected => "not affected",
        _ => "unsupported; requests to disable return EOPNOTSUPP" });
}
pub fn apply_current(task: bool) {
    let Some(plan) = PLAN.get().copied() else { return; }; // early idle ticks
    let desired = plan.desired(task);
    match plan.backend {
        Backend::SpecCtrl => {
            let mask = 4 | if plan.psfd { 1 << 7 } else { 0 };
            super::spec_ctrl::update(mask, if desired { mask } else { 0 });
        }
        Backend::VirtualSpecCtrl => {
            let cpu = super::cpu_index();
            let old = if VIRT_VALID[cpu].load(Ordering::Acquire) { VIRT_VALUE[cpu].load(Ordering::Relaxed) }
                else { unsafe { super::spec_ctrl::read_msr(0xc001_011f) } };
            let new = (old & !4) | if desired { 4 } else { 0 };
            if new != old {
                unsafe { super::spec_ctrl::write_msr(0xc001_011f, new); }
                assert_eq!(unsafe { super::spec_ctrl::read_msr(0xc001_011f) } & 4, new & 4);
            }
            VIRT_VALUE[cpu].store(new, Ordering::Relaxed); VIRT_VALID[cpu].store(true, Ordering::Release);
            if plan.psfd { super::spec_ctrl::update(1 << 7, if desired { 1 << 7 } else { 0 }); }
        }
        _ => {}
    }
}
pub fn testable_ssbs() -> bool { false }

pub fn prepare_task(task: bool, _context: &mut super::TaskContext) { apply_current(task); }
