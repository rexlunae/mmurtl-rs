//! Return gateways are patched once on the BSP, before any AP starts and
//! before image aliases are sealed. No runtime text patching is permitted.
use crate::security::{return_thunk as layout,Vulnerability as V};
use layout::Class;
use core::sync::atomic::{AtomicU8,Ordering};
use x86_64::{VirtAddr,PhysAddr,structures::paging::{Page,PhysFrame,page_table::PageTableFlags as Flags}};
static CLASS:AtomicU8=AtomicU8::new(Class::Unknown as u8);
static ENABLED:core::sync::atomic::AtomicBool=core::sync::atomic::AtomicBool::new(false);
unsafe extern "C" {
    static __x86_return_patch:u8;
    static __x86_untrain_patch:u8;
    fn __x86_plain_return();
}
pub fn compatible(cpu:usize)->bool {
    !ENABLED.load(Ordering::Acquire)||crate::security::caps(cpu).is_some_and(|c|layout::class(c) as u8==CLASS.load(Ordering::Acquire))
}
pub fn report(plan:crate::security::x86_predictor::Plan) {
    let common=crate::security::common();let policy=crate::security::policy();
    let class=CLASS.load(Ordering::Acquire);
    if policy.enabled(V::Srso) && common.not_affected&V::Srso.bit()==0 {
        if matches!(class,1|2)&&layout::microcode_return_barrier(common)&&plan.ibpb&&plan.sibling_safe {
            crate::security::installed(V::Srso,"AMD safe RET + extended IBPB across user identities");
        } else {
            crate::security::unavailable(V::Srso,if matches!(class,1|2) {"safe RET active; extended IBPB microcode or sibling isolation missing"} else {"generic return hardening; CPU-specific SRSO protection unverified"});
        }
    }
    if policy.enabled(V::Retbleed) {
        if (matches!(class,1|2)||matches!(plan.mode,crate::security::x86_predictor::Mode::LegacyIbrs|crate::security::x86_predictor::Mode::EnhancedIbrs))&&plan.sibling_safe {
            crate::security::installed(V::Retbleed,"safe return backend or Intel IBRS + RSB fill");
        } else {crate::security::unavailable(V::Retbleed,"generic safe-return sequence; CPU-specific protection unverified");}
    }
}
pub fn prepare_boot() {
    let caps=crate::security::caps(0).expect("BSP capabilities");
    let class=layout::class(caps);
    let policy=crate::security::policy();
    let enabled=policy.enabled(V::Retbleed)||policy.enabled(V::Srso)||policy.enabled(V::Its);
    CLASS.store(class as u8,Ordering::Release);ENABLED.store(enabled,Ordering::Release);
    let ret=core::ptr::addr_of!(__x86_return_patch) as u64;
    let untrain=core::ptr::addr_of!(__x86_untrain_patch) as u64;
    if !enabled {
        unsafe {patch(ret,__x86_plain_return as *const () as u64);}
        crate::serial::write_line("[SPEC] return backend: optional controls off; plain RET + SLS trap");
        return;
    }
    if !matches!(class,Class::Zen12|Class::Zen34) {
        crate::serial::write_line("[SPEC] return backend: generic CALL/LFENCE safe RET (CPU-specific guarantees reported separately)");
        return;
    }
    // Keep the code within rel32 reach of the gateways. Reserving only two
    // pages gives Zen3/4 the exact 2-MiB virtual alias pattern regardless of
    // the bootloader's 4-KiB image placement.
    let origin=ret&!0x1fffff;
    let draw=(crate::random::early_u64() as usize)&511;
    let base=(0..512).map(|i|origin.wrapping_add((((i+draw)%512) as u64).wrapping_sub(256)<<21))
        .filter(|base|*base>=0xffff800000000000 && *base<0xffffc00000000000)
        .find(|base| (0..512).all(|page|super::page_table::translate_virtual(VirtAddr::new(base+page*4096)).is_none()))
        .expect("no nearby return code region");
    crate::memory::heap::with_frame_allocator(|fa| {
        assert!(fa.free_count()>=16,"return code frame budget");
        let pa=fa.allocate_zeroed_frame().expect("return code");
        let pb=fa.allocate_zeroed_frame().expect("return code");
        let sites=unsafe {
            let a=core::slice::from_raw_parts_mut(super::memory::phys_to_virt(pa),4096);
            let b=core::slice::from_raw_parts_mut(super::memory::phys_to_virt(pb),4096);
            layout::build(class,a,b)
        };
        let mut adapter=super::page_table::BumpFrameAllocator::new(fa);
        for (va,physical) in [(base,pa),(base+0x104000,pb)] {
            unsafe {
                super::page_table::set_page_readonly(super::page_table::phys_to_virt(PhysAddr::new(physical)),&mut adapter).expect("seal return code alias");
                super::page_table::map_page(Page::containing_address(VirtAddr::new(va)),PhysFrame::containing_address(PhysAddr::new(physical)),Flags::PRESENT,&mut adapter).expect("map return code RX");
                let code=super::page_table::query_page(VirtAddr::new(va)).unwrap();
                assert!(!code.intersects(Flags::WRITABLE|Flags::NO_EXECUTE|Flags::USER_ACCESSIBLE));
                let alias=super::page_table::query_page(super::page_table::phys_to_virt(PhysAddr::new(physical))).unwrap();
                assert!(!alias.intersects(Flags::WRITABLE|Flags::USER_ACCESSIBLE)&&alias.contains(Flags::NO_EXECUTE));
            }
        }
        unsafe {patch(ret,base+sites.entry as u64);patch(untrain,base+sites.untrain as u64);}
    }).expect("frame allocator initialized");
    crate::serial::write_line(match class {Class::Zen12=>"[SPEC] return backend: Zen1/2 overlapping safe RET",_=>"[SPEC] return backend: Zen3/4 2-MiB alias safe RET"});
}
unsafe fn patch(site:u64,target:u64) {
    let bytes=layout::jump(site,target).expect("return gateway rel32 range");
    assert_eq!(site&7,0);
    let physical=super::page_table::translate_virtual(VirtAddr::new(site)).expect("return gateway mapping");
    let alias=super::page_table::phys_to_virt(physical).as_mut_ptr::<u64>();
    alias.write_volatile(u64::from_le_bytes(bytes));
    let _=core::arch::x86_64::__cpuid(0);
}
core::arch::global_asm!(r#"
.text
.balign 64
.global __x86_return_thunk
__x86_return_thunk:
    endbr64
.balign 8, 0x90
.global __x86_return_patch
__x86_return_patch:
    jmp __x86_generic_return
    .fill 3, 1, 0xcc
.balign 64
.global __x86_untrain_entry
__x86_untrain_entry:
    endbr64
.balign 8, 0x90
.global __x86_untrain_patch
__x86_untrain_patch:
    jmp __x86_return_thunk
    .fill 3, 1, 0xcc
.balign 64
__x86_generic_return:
    endbr64
    call .Lsafe_return
    ud2
    int3
.balign 64
    .fill 32, 1, 0xcc
.Lsafe_return:
    lea rsp, [rsp + 8]
    lfence
    ret
    int3
.balign 64
    .fill 32, 1, 0xcc
.global __x86_plain_return
__x86_plain_return:
    ret
    int3
"#);
