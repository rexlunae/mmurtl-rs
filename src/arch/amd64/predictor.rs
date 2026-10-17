//! Compiler external indirect-branch thunks.
use core::sync::atomic::{AtomicBool,AtomicU32,AtomicU64,AtomicU8,Ordering};
use crate::security::{Vulnerability as V,feature as F};
use crate::security::x86_predictor::{Plan,Mode};
static PLAN:spin::Once<Plan>=spin::Once::new();
#[no_mangle]
static X86_PRED_CPUS:AtomicU64=AtomicU64::new(0);
#[no_mangle]
static X86_PRED_LAPIC:AtomicU64=AtomicU64::new(0);
static APPLIED:[AtomicU8;64]=[const {AtomicU8::new(0)};64];
static LAST_USER:[AtomicU32;64]=[const {AtomicU32::new(0)};64];
static SWITCH_IBPB:AtomicU64=AtomicU64::new(0);
static SWITCH_RSB:AtomicU64=AtomicU64::new(0);
#[no_mangle]
static X86_ENTRY_SPEC_BITS:AtomicU32=AtomicU32::new(0);
#[no_mangle]
static X86_ENTRY_AUTOIBRS:AtomicBool=AtomicBool::new(false);
#[no_mangle]
static X86_LEGACY_IBRS:AtomicBool=AtomicBool::new(false);
#[no_mangle]
static X86_ENTRY_BHB:AtomicBool=AtomicBool::new(false);
#[no_mangle]
static X86_ENTRY_IBHF:AtomicBool=AtomicBool::new(false);
#[no_mangle]
static X86_ENTRY_RSB:AtomicBool=AtomicBool::new(false);
#[no_mangle]
static X86_USER_RSB_COUNT:AtomicU64=AtomicU64::new(0);
pub fn init() {
    let admitted=crate::security::freeze();
    let intel=(0..64).filter(|cpu|admitted&(1<<cpu)!=0).all(|cpu|crate::security::caps(cpu).is_some_and(|caps|caps.vendor==*b"GenuineIntel"));
    let p=Plan::select(crate::security::common(),crate::security::policy(),intel,crate::topology::isolating());
    PLAN.call_once(||p);X86_PRED_LAPIC.store(super::apic::entry_mmio_base(),Ordering::Release);
    X86_PRED_CPUS.store(admitted,Ordering::Release);
    X86_ENTRY_SPEC_BITS.store(p.set as u32,Ordering::Release);
    X86_ENTRY_AUTOIBRS.store(p.mode==Mode::AutoIbrs,Ordering::Release);
    X86_LEGACY_IBRS.store(p.mode==Mode::LegacyIbrs,Ordering::Release);
    X86_ENTRY_BHB.store(p.bhb_loop,Ordering::Release);X86_ENTRY_IBHF.store(p.ibhf,Ordering::Release);X86_ENTRY_RSB.store(p.rsb,Ordering::Release);
    apply_current();
    let targets=crate::scheduler::boot_cpu_targets();
    for (cpu,id) in targets.iter().enumerate() {if cpu!=super::cpu_index() {if let Some(id)=id {crate::arch::send_resched_ipi(*id);}}}
    for cpu in 0..64 {
        if admitted&(1<<cpu)==0 {continue;}
        for _ in 0..1000 {if APPLIED[cpu].load(Ordering::Acquire)==2 {break;}super::apic::pit_wait_ms(1);}
        assert_eq!(APPLIED[cpu].load(Ordering::Acquire),2,"predictor CPU control rendezvous timed out");
    }
    crate::serial::write_str("[SPEC] indirect branches: compiler retpolines + landing checks; ");
    crate::serial::write_line(match p.mode {Mode::EnhancedIbrs=>"eIBRS",Mode::AutoIbrs=>"AMD AutoIBRS",Mode::LegacyIbrs=>"legacy IBRS entry/exit",Mode::Retpolines=>"hardware IBRS unavailable",Mode::Off=>"optional predictor controls off (compiler hardening retained)"});
    if crate::security::policy().enabled(V::SpectreV2) {
        if p.sibling_safe&&p.ibpb {crate::security::installed(V::SpectreV2,"retpolines, RSB fill, user IBPB; STIBP or isolated siblings");}
        else {crate::security::unavailable(V::SpectreV2,if !p.sibling_safe {"retpolines active; SMT enabled without common STIBP"} else {"kernel retpolines active; user-to-user IBPB unavailable"});}
    }
    if crate::security::policy().enabled(V::Bhi) {
        if p.set&(1<<10)!=0 {crate::security::installed(V::Bhi,"BHI_DIS_S + IBHF");}
        else if p.bhb_loop {crate::security::installed(V::Bhi,"Intel long BHB clear + retpolines");}
        else {crate::security::unavailable(V::Bhi,"retpolines active; no verified history-clearing control");}
    }
    if intel&&crate::security::policy().enabled(V::Its) {crate::security::installed(V::Its,"retpolines + all near RETs in upper cacheline halves; current microcode required for IBPB");}
    crate::serial::write_line(if p.ibpb {"[SPEC] IBPB: different user task identity on this CPU (root reuse cannot skip barrier)"} else {"[SPEC] IBPB unavailable or disabled; cross-task predictor isolation not guaranteed"});
    super::returns::report(p);
}
pub fn apply_current() {
    let Some(p)=PLAN.get().copied() else {return;};
    crate::arch::without_interrupts(|| {
        let cpu=super::cpu_index();
        if X86_PRED_CPUS.load(Ordering::Acquire)&(1<<cpu)==0||APPLIED[cpu].compare_exchange(0,1,Ordering::AcqRel,Ordering::Acquire).is_err() {return;}
        super::spec_ctrl::update(p.clear,p.set);
        if crate::security::common().features&F::AUTOIBRS!=0 {
            unsafe {
                let old=super::spec_ctrl::read_msr(0xc0000080);
                let new=(old&!(1<<21))|if p.mode==Mode::AutoIbrs {1<<21} else {0};
                super::spec_ctrl::write_msr(0xc0000080,new);
                assert_eq!(super::spec_ctrl::read_msr(0xc0000080)&(1<<21),new&(1<<21));
            }
        }
        APPLIED[cpu].store(2,Ordering::Release);
    });
}
pub fn context_switch(cpu:usize,user:bool,task:u32) {
    let Some(p)=PLAN.get().copied() else {return;};
    if user&&LAST_USER[cpu].swap(task,Ordering::Relaxed)!=task&&p.ibpb {
        unsafe {x86_user_barrier();}
        SWITCH_IBPB.fetch_add(1,Ordering::Relaxed);
        if p.bhb_loop {unsafe {x86_clear_bhb();}}
        if p.ibhf {unsafe {core::arch::asm!(".byte 0xf3,0x48,0x0f,0x1e,0xf8",options(nomem,nostack));}}
    }
    if p.rsb {unsafe {x86_fill_rsb();}SWITCH_RSB.fetch_add(1,Ordering::Relaxed);}
}
unsafe extern "C" {fn x86_clear_bhb();fn x86_fill_rsb();fn x86_user_barrier();}
pub fn verify_usage()->bool {
    let Some(p)=PLAN.get() else {return false;};
    let entry=X86_USER_RSB_COUNT.load(Ordering::Acquire);
    let switches=SWITCH_RSB.load(Ordering::Acquire);
    let ibpb=SWITCH_IBPB.load(Ordering::Acquire);
    crate::serial::write_str("[SPEC TEST] entry RSB fills=");crate::serial::write_dec(entry);
    crate::serial::write_str(", switch RSB fills=");crate::serial::write_dec(switches);
    crate::serial::write_str(", user IBPB barriers=");crate::serial::write_dec(ibpb);crate::serial::write_str("\n");
    (!p.rsb||entry>0&&switches>0)&&(!p.ibpb||ibpb>0)
}
core::arch::global_asm!(r#"
.text
.global x86_user_barrier
x86_user_barrier:
    endbr64
    mov ecx, 0x49
    mov eax, 1
    xor edx, edx
    wrmsr
    // Restore the safe return predictor before the first post-IBPB RET.
    call __x86_untrain_entry
    jmp __x86_return_thunk
.global x86_predictor_enter
x86_predictor_enter:
    endbr64
    // Parked CPUs keep NMI/MC gates but may lack the admitted common ISA.
    // Use the same xAPIC-to-index mapping as cpu_index, before any MSR.
    mov rcx, qword ptr [rip + X86_PRED_LAPIC]
    xor eax, eax
    test rcx, rcx
    jz .Lentry_cpu_known
    mov ecx, dword ptr [rcx + 0x20]
    shr ecx, 24
    lea rdx, [rip + APIC_TO_CPU]
    mov eax, dword ptr [rdx + rcx * 4]
.Lentry_cpu_known:
    cmp eax, 64
    jae .Lentry_history_done
    mov rcx, qword ptr [rip + X86_PRED_CPUS]
    bt rcx, rax
    jnc .Lentry_history_done
    // Preserve RDI/RSI (saved-frame pointer/CPL), clobber only AX/CX/DX.
    mov ecx, dword ptr [rip + X86_ENTRY_SPEC_BITS]
    test ecx, ecx
    jz .Lentry_spec_done
    push rcx
    mov ecx, 0x48
    rdmsr
    or eax, dword ptr [rsp]
    add rsp, 8
    wrmsr
.Lentry_spec_done:
    cmp byte ptr [rip + X86_ENTRY_AUTOIBRS], 0
    je .Lentry_auto_done
    mov ecx, 0xc0000080
    rdmsr
    or eax, (1 << 21)
    wrmsr
.Lentry_auto_done:
    call __x86_untrain_entry
    test esi, esi
    jz .Lentry_history_done
    cmp byte ptr [rip + X86_ENTRY_BHB], 0
    je .Lentry_bhb_done
    call x86_clear_bhb
.Lentry_bhb_done:
    cmp byte ptr [rip + X86_ENTRY_IBHF], 0
    je .Lentry_ibhf_done
    .byte 0xf3,0x48,0x0f,0x1e,0xf8
.Lentry_ibhf_done:
    cmp byte ptr [rip + X86_ENTRY_RSB], 0
    je .Lentry_history_done
    call x86_fill_rsb
    lock inc qword ptr [rip + X86_USER_RSB_COUNT]
.Lentry_history_done:
    // Resolve admission, CPL and optional-control branches before compiled
    // kernel code can execute. CPUID is serializing even on older CPUs
    // where LFENCE dispatch serialization is not guaranteed.
    push rbx
    xor eax, eax
    cpuid
    pop rbx
    jmp __x86_return_thunk
    int3
.global x86_clear_bhb
x86_clear_bhb:
    endbr64
    mov ecx, 12
    call .Lbhb_outer
    jmp .Lbhb_complete
    .balign 64
.Lbhb_outer:
    call .Lbhb_inner
    .balign 32, 0x90
    ret
    int3
    .balign 64
.Lbhb_inner:
    mov eax, 7
.Lbhb_jump:
    jmp .Lbhb_count
    nop
.Lbhb_count:
    sub eax, 1
    jnz .Lbhb_jump
    sub ecx, 1
    jnz .Lbhb_outer
    .balign 32, 0x90
    ret
    int3
.Lbhb_complete:
    lfence
    jmp __x86_return_thunk
    int3
.global x86_fill_rsb
x86_fill_rsb:
    endbr64
    mov ecx, 64
.Lrsb_push:
    call .Lrsb_next
.Lrsb_sink:
    pause
    lfence
    jmp .Lrsb_sink
.Lrsb_next:
    dec ecx
    jnz .Lrsb_push
    add rsp, 512
    jmp __x86_return_thunk
    int3
"#);
#[cfg(feature="test-retpoline-landing")]
pub fn test_landing()->! {
    unsafe extern "C" {fn retpoline_landing_probe()->!;}
    unsafe {retpoline_landing_probe();}
}
#[cfg(feature="test-retpoline-landing")]
core::arch::global_asm!(r#"
.text
.global retpoline_landing_probe
retpoline_landing_probe:
    endbr64
    lea rax, [rip + retpoline_non_landing]
    jmp __x86_indirect_thunk_rax
retpoline_non_landing:
    nop
    ud2
"#);
core::arch::global_asm!(r#"
.text
.macro INDIRECT_THUNK reg
.balign 64
.global __x86_indirect_thunk_\reg
.type __x86_indirect_thunk_\reg, @function
__x86_indirect_thunk_\reg:
    endbr64
    pushfq
    cmp byte ptr [rip + X86_RETPOLINE_LANDING], 0
    je .Llanding_ok\@
    cmp dword ptr [\reg], 0xfa1e0ff3
    jne .Llanding_bad\@
.Llanding_ok\@:
    popfq
    call .Linstall_target\@
.Lprediction_sink\@:
    pause
    lfence
    jmp .Lprediction_sink\@
.Linstall_target\@:
    mov [rsp], \reg
    jmp __x86_return_thunk
    int3
.Llanding_bad\@:
    popfq
    mov r11, \reg
    jmp __x86_retpoline_reject
.size __x86_indirect_thunk_\reg, .-__x86_indirect_thunk_\reg
.endm
.global __x86_retpoline_reject
__x86_retpoline_reject:
    ud2
    int3
INDIRECT_THUNK rax
INDIRECT_THUNK rcx
INDIRECT_THUNK rdx
INDIRECT_THUNK rbx
INDIRECT_THUNK rbp
INDIRECT_THUNK rsi
INDIRECT_THUNK rdi
INDIRECT_THUNK r8
INDIRECT_THUNK r9
INDIRECT_THUNK r10
INDIRECT_THUNK r11
INDIRECT_THUNK r12
INDIRECT_THUNK r13
INDIRECT_THUNK r14
INDIRECT_THUNK r15
"#);
