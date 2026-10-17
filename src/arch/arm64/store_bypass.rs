use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use crate::security::store_bypass::{Plan, Backend, Architecture, Mode};
static PLAN: spin::Once<Plan> = spin::Once::new();
static CONFIGURED: [AtomicBool; 64] = [const { AtomicBool::new(false) }; 64];
static FW_VALUE: [AtomicU8; 64] = [const { AtomicU8::new(2) }; 64];
pub fn plan() -> Plan { *PLAN.get().expect("store-bypass policy not initialized") }
pub fn init() {
    let plan = Plan::select(crate::security::common(), crate::security::policy(), Architecture::Arm64);
    PLAN.call_once(|| plan);
    apply_current(false);
    if plan.supported() && plan.mode != Mode::Off {
        crate::security::installed(crate::security::Vulnerability::Ssb,
            if plan.mode == Mode::On { "forced store-bypass disable" } else { "per-task store-bypass policy" });
    }
    crate::serial::write_str("[SPEC] spec_store_bypass: ");
    crate::serial::write_str(match plan.mode { Mode::Off => "off ", Mode::On => "on ", Mode::PerTask => "per-task " });
    crate::serial::write_line(match plan.backend { Backend::Ssbs => "SSBS (saved SPSR and kernel entry default)",
        Backend::Firmware => "SMCCC_ARCH_WORKAROUND_2", Backend::NotAffected => "not affected",
        _ => "unsupported; requests to disable return EOPNOTSUPP" });
}
/// No stack-frame access here: a running task's old context_ptr is stale.
pub fn apply_current(task: bool) {
    let Some(plan) = PLAN.get().copied() else { return; };
    let cpu = super::cpu_index();
    if plan.backend == Backend::Ssbs && !CONFIGURED[cpu].load(Ordering::Relaxed) {
        unsafe {
            let mut sctlr: u64;
            core::arch::asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack));
            // SSBS=0 prohibits bypass. Default every kernel exception entry
            // to safe when policy is enabled, independently of user SSBS.
            sctlr = (sctlr & !(1 << 44)) | if plan.mode == Mode::Off { 1 << 44 } else { 0 };
            core::arch::asm!("msr sctlr_el1, {}", "isb", in(reg) sctlr, options(nostack));
        }
        CONFIGURED[cpu].store(true, Ordering::Release);
    }
    if plan.backend == Backend::Firmware {
        let desired = u8::from(plan.desired(task));
        if FW_VALUE[cpu].load(Ordering::Relaxed) != desired {
            super::cpu_features::smccc(0x8000_7fff, desired as u64);
            FW_VALUE[cpu].store(desired, Ordering::Relaxed);
        }
    }
}
pub fn prepare_task(task: bool, context: &mut super::TaskContext) {
    apply_current(task);
    let Some(plan) = PLAN.get().copied() else { return; };
    let available = crate::security::common().features & crate::security::feature::SSBS != 0;
    context.spsr = crate::security::store_bypass::ssbs_frame(context.spsr, plan.desired(task), available);
    if available { assert_eq!(context.spsr & (1 << 12) == 0, plan.desired(task)); }

}
/// Called after scheduler_unlock, with a valid exception frame and IRQs
/// masked, on every return to EL0 (including non-scheduling device IRQs).
#[no_mangle]
pub unsafe extern "C" fn arm64_user_return_policy(frame: *mut super::TaskContext) {
    if (*frame).spsr & 15 == 0 {
        let task = crate::scheduler::current_store_bypass_flag();
        prepare_task(task, &mut *frame);
    }
    super::predictor::arm64_predictor_user_return(frame);
    super::pan::prepare_return(&mut *frame);
}
pub fn testable_ssbs() -> bool {
    plan().backend == Backend::Ssbs && crate::security::common().features & crate::security::feature::SSBS2 != 0
}
