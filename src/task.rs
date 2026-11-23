use core::arch::global_asm;
use core::mem::size_of;

use crate::shell::puts;

const STACK_SIZE: usize = 4096;
const CTX_FRAME_SIZE: usize = 112;
const ROUNDS: usize = 3;

#[repr(align(16))]
struct Stack([u8; STACK_SIZE]);

static mut STACK_A: Stack = Stack([0; STACK_SIZE]);
static mut STACK_B: Stack = Stack([0; STACK_SIZE]);

static mut SHELL_SP: usize = 0;
static mut SP_A: usize = 0;
static mut SP_B: usize = 0;

global_asm!(
    r#"
    .section .text
    .global __switch

__switch:
    addi sp, sp, -112
    sd ra, 0(sp)
    sd s0, 8(sp)
    sd s1, 16(sp)
    sd s2, 24(sp)
    sd s3, 32(sp)
    sd s4, 40(sp)
    sd s5, 48(sp)
    sd s6, 56(sp)
    sd s7, 64(sp)
    sd s8, 72(sp)
    sd s9, 80(sp)
    sd s10, 88(sp)
    sd s11, 96(sp)

    sd sp, 0(a0)
    mv sp, a1
    ld ra, 0(sp)
    ld s0, 8(sp)
    ld s1, 16(sp)
    ld s2, 24(sp)
    ld s3, 32(sp)
    ld s4, 40(sp)
    ld s5, 48(sp)
    ld s6, 56(sp)
    ld s7, 64(sp)
    ld s8, 72(sp)
    ld s9, 80(sp)
    ld s10, 88(sp)
    ld s11, 96(sp)
    addi sp, sp, 112
    ret

"#
);

unsafe extern "C" {
    fn __switch(old_sp: *mut usize, new_sp: usize);
}

fn init_stack(stack: *mut u8, entry: extern "C" fn() -> !) -> usize {
    let top = (stack as usize + STACK_SIZE) & !0xF;
    let sp = top - CTX_FRAME_SIZE;
    let frame = sp as *mut usize;
    unsafe {
        for i in 0..(CTX_FRAME_SIZE / size_of::<usize>()) {
            frame.add(i).write(0);
        }
        frame.write(entry as usize);
    }
    sp
}

extern "C" fn task_a() -> ! {
    for _ in 0..ROUNDS {
        puts("[Task A]\n");
        unsafe {
            __switch(&raw mut SP_A, SP_B);
        }
    }
    loop {
        unsafe {
            __switch(&raw mut SP_A, SHELL_SP);
        }
    }
}

extern "C" fn task_b() -> ! {
    for i in 0..ROUNDS {
        puts("[Task B]\n");
        if i + 1 < ROUNDS {
            unsafe {
                __switch(&raw mut SP_B, SP_A);
            }
        } else {
            unsafe {
                __switch(&raw mut SP_B, SHELL_SP);
            }
        }
        
    }
    loop {
        unsafe {
            __switch(&raw mut SP_B, SHELL_SP);
        }
    }
}

pub fn demo() {
    unsafe {
        SP_A = init_stack((&raw mut STACK_A).cast::<u8>(), task_a);
        SP_B = init_stack((&raw mut STACK_B).cast::<u8>(), task_b);
        __switch(&raw mut SHELL_SP, SP_A);
        puts("sched done\n");
    }
}
