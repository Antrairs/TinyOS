use core::arch::{asm, global_asm};
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::shell::uart_isr;

const TICK_INTERVAL: u64 = 1_000_000;
const SCAUSE_S_TIMER: usize = (1usize << 63) | 5;
const SCAUSE_S_EXT: usize = (1usize << 63) | 9;

const PLIC_BASE: usize = 0x0c00_0000; //
const PLIC_SENABLE: usize = PLIC_BASE + 0x2080; // hart 0 S-mode 中断使能
const PLIC_SPRIORITY: usize = PLIC_BASE + 0x20_1000; // hart 0 S-mode 阈值
const PLIC_SCLAIM: usize = PLIC_BASE + 0x20_1004; // hart 0 S-mode 认领/完成
const UART0_IRQ: u32 = 10;

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

pub fn irq_save() -> usize {
    let sstatus: usize;
    unsafe {
        asm!("csrrci {}, sstatus, 2", out(reg) sstatus);
    }
    sstatus & 2
}

pub fn irq_restore(saved: usize) {
    if saved != 0 {
        enable_irq();
    }
}

pub fn init_uart_irq() {
    const UART0_IER: usize = 0x1000_0001;
    unsafe {
        write_volatile(UART0_IER as *mut u8, 1); // 开 UART 接收中断
        write_volatile((PLIC_BASE + (UART0_IRQ as usize) * 4) as *mut u32, 1); // IRQ 10 优先级=1
        write_volatile(PLIC_SENABLE as *mut u32, 1u32 << UART0_IRQ); // S-mode 允许 IRQ 10
        write_volatile(PLIC_SPRIORITY as *mut u32, 0); // 阈值=0 全部放行
        asm!("csrs sie, {}", in(reg) 1usize << 9); // 开 CPU SEIE (bit 9)
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
    } else if scause == SCAUSE_S_EXT {
        let irq = unsafe { read_volatile(PLIC_SCLAIM as *const u32) };
        if irq == UART0_IRQ {
            uart_isr();
            if irq != 0 {
                unsafe {
                    write_volatile(PLIC_SCLAIM as *mut u32, irq);
                }
            }
        }
    }
}
