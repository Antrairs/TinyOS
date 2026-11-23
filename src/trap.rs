use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicUsize, Ordering};

const TICK_INTERVAL: u64 = 1_000_000;
const SCAUSE_S_TIMER: usize = (1usize << 63) | 5;

static TICKS: AtomicUsize = AtomicUsize::new(0);

global_asm!(
    r#"
    .section .text
    .align 2
    .global __trap_entry
__trap_entry:
    addi sp, sp, -144
    sd ra, 0(sp)
    sd t0, 8(sp)
    sd t1, 16(sp)
    sd t2, 24(sp)
    sd t3, 32(sp)
    sd t4, 40(sp)
    sd t5, 48(sp)
    sd t6, 56(sp)
    sd a0, 64(sp)
    sd a1, 72(sp)
    sd a2, 80(sp)
    sd a3, 88(sp)
    sd a4, 96(sp)
    sd a5, 104(sp)
    sd a6, 112(sp)
    sd a7, 120(sp)

    csrr t0, sepc
    sd t0, 128(sp)
    csrr t1, sstatus
    sd t1, 136(sp)

    csrr a0, scause
    call trap_handler

    ld t1, 136(sp)
    csrw sstatus, t1
    ld t0, 128(sp)
    csrw sepc, t0

    ld ra, 0(sp)
    ld t0, 8(sp)
    ld t1, 16(sp)
    ld t2, 24(sp)
    ld t3, 32(sp)
    ld t4, 40(sp)
    ld t5, 48(sp)
    ld t6, 56(sp)
    ld a0, 64(sp)
    ld a1, 72(sp)
    ld a2, 80(sp)
    ld a3, 88(sp)
    ld a4, 96(sp)
    ld a5, 104(sp)
    ld a6, 112(sp)
    ld a7, 120(sp)
    addi sp, sp, 144
    sret
"#
);

unsafe extern "C" {
    fn __trap_entry();
}

pub fn read_time() -> u64 {
    let t: u64;
    unsafe {
        asm!("csrr {}, time", out(reg) t);
    }
    t
}

pub fn set_timer(next: u64) {
    unsafe {
        asm!(
            "ecall",
            in("a7") 0x5449_4D45usize,
            in("a6") 0usize,
            in("a0") next,
            lateout("a0") _,
            lateout("a1") _,
        );
    }
}

pub fn ticks() -> usize {
    TICKS.load(Ordering::Relaxed)
}

pub fn enable_irq() {
    unsafe {
        asm!("csrsi sstatus, 2");
    }
}

pub fn disable_irq() {
    unsafe {
        asm!("csrci sstatus, 2");
    }
}

pub fn start_timer() {
    TICKS.store(0, Ordering::Relaxed);
    unsafe {
        let entry = __trap_entry as usize;
        asm!("csrw stvec, {}", in(reg) entry);
        asm!("csrs sie, {}", in(reg) 1usize << 5);
    }
    set_timer(read_time() + TICK_INTERVAL);
}

pub fn stop_timer() {
    disable_irq();
    unsafe {
        asm!("csrc sie, {}", in(reg) 1usize << 5);
    }
}

#[unsafe(no_mangle)]
extern "C" fn trap_handler(scause: usize) {
    if scause == SCAUSE_S_TIMER {
        set_timer(read_time() + TICK_INTERVAL);
        TICKS.fetch_add(1, Ordering::Relaxed);
        crate::task::on_tick();
    }
}
