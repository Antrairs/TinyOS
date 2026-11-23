use core::arch::global_asm;
use core::mem::size_of;

use crate::shell::puts;

const STACK_SIZE: usize = 4096;
const CTX_FRAME_SIZE: usize = 112;

#[repr(align(16))]
struct Stack([u8; STACK_SIZE]);

const MAX_TASKS: usize = 8;

#[derive(Copy, Clone, PartialEq)]
enum TaskState {
    Unused,
    Ready,
    Running,
}

#[derive(Copy, Clone)]
struct Tcb {
    sp: usize,
    state: TaskState,
    prio: u8,
    name: &'static str,
}

impl Tcb {
    const fn empty() -> Self {
        Self {
            sp: 0,
            state: TaskState::Unused,
            prio: 0,
            name: "",
        }
    }
}

struct Kernel {
    tcbs: [Tcb; MAX_TASKS],
    used_mask: u32,
    current: Option<usize>,
    shell_sp: usize,
}

impl Kernel {
    const fn new() -> Self {
        Self {
            tcbs: [Tcb::empty(); MAX_TASKS],
            used_mask: 0,
            current: None,
            shell_sp: 0,
        }
    }
}

static mut KERNEL: Kernel = Kernel::new();
static mut STACKS: [Stack; MAX_TASKS] = [const { Stack([0; STACK_SIZE]) }; MAX_TASKS];

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

    .global task_trampoline
task_trampoline:
    mv a0, s1
    jalr s0
    call task_exit
"#
);

unsafe extern "C" {
    fn __switch(old_sp: *mut usize, new_sp: usize);
    fn task_trampoline();
}

fn init_stack(tid: usize, entry: extern "C" fn(usize), arg: usize) -> usize {
    let stack = unsafe { (&raw mut STACKS[tid]).cast::<u8>() };
    let top = (stack as usize + STACK_SIZE) & !0xF;
    let sp = top - CTX_FRAME_SIZE;
    let frame = sp as *mut usize;
    unsafe {
        for i in 0..(CTX_FRAME_SIZE / size_of::<usize>()) {
            frame.add(i).write(0);
        }
        frame.add(0).write(task_trampoline as usize);
        frame.add(1).write(entry as usize);
        frame.add(2).write(arg);
    }
    sp
}

fn task_create(
    name: &'static str,
    entry: extern "C" fn(usize),
    prio: u8,
    arg: usize,
) -> Option<usize> {
    unsafe {
        let tid = (!KERNEL.used_mask).trailing_zeros() as usize;
        if tid >= MAX_TASKS {
            return None;
        }
        KERNEL.used_mask |= 1 << tid;
        let sp = init_stack(tid, entry, arg);
        KERNEL.tcbs[tid] = Tcb {
            sp,
            state: TaskState::Ready,
            prio,
            name,
        };
        Some(tid)
    }
}

fn task_delete(tid: usize) {
    if tid >= MAX_TASKS {
        return;
    }
    unsafe {
        KERNEL.used_mask &= !(1 << tid);
        KERNEL.tcbs[tid].state = TaskState::Unused;
    }
}

fn schedule() {
    unsafe {
        let prev = KERNEL.current;

        // 从下一个槽位起顺扫一圈 找第一个 Ready 任务
        let start = match prev {
            Some(c) => (c + 1) % MAX_TASKS,
            None => 0,
        };

        let mut next = None;
        for offset in 0..MAX_TASKS {
            let tid = (start + offset) % MAX_TASKS;
            let state = KERNEL.tcbs[tid].state;
            if state == TaskState::Ready {
                next = Some(tid);
                break;
            }
        }

        // 如果只剩当前任务自己直接恢复 Running
        if next == prev {
            if let Some(c) = prev {
                KERNEL.tcbs[c].state = TaskState::Running;
            }
            return;
        }

        // 决定旧 sp 存到哪
        let mut dummy_sp: usize = 0;
        let old_sp = match prev {
            None => &raw mut KERNEL.shell_sp,
            Some(c) => {
                let state = KERNEL.tcbs[c].state;
                if state == TaskState::Unused {
                    &raw mut dummy_sp
                } else {
                    &raw mut KERNEL.tcbs[c].sp
                }
            }
        };

        // 切去下一个任务; 若全部退出则切回 Shell
        let new_sp = match next {
            Some(tid) => {
                KERNEL.tcbs[tid].state = TaskState::Running;
                KERNEL.current = Some(tid);
                KERNEL.tcbs[tid].sp
            }
            None => {
                KERNEL.current = None;
                KERNEL.shell_sp
            }
        };
        __switch(old_sp, new_sp);
    }
}

fn task_yield() {
    unsafe {
        if let Some(cur) = KERNEL.current {
            let state = KERNEL.tcbs[cur].state;
            if state == TaskState::Running {
                KERNEL.tcbs[cur].state = TaskState::Ready;
            }
        }
    }
    schedule();
}

#[unsafe(no_mangle)]
extern "C" fn task_exit() -> ! {
    unsafe {
        if let Some(cur) = KERNEL.current {
            task_delete(cur);
        }
    }
    schedule();
    loop {}
}

fn current_name() -> &'static str {
    unsafe {
        match KERNEL.current {
            Some(cur) => KERNEL.tcbs[cur].name,
            None => "shell",
        }
    }
}

extern "C" fn worker_task(rounds: usize) {
    for _ in 0..rounds {
        puts("[");
        puts(current_name());
        puts("] working\n");
        task_yield();
    }
    puts("[");
    puts(current_name());
    puts("] exiting\n");
}

extern "C" fn heartbeat_task(beats: usize) {
    for _ in 0..beats {
        puts("\x1b[32m[HeartBeat] alive\x1b[0m\n");
        task_yield();
    }
    puts("\x1b[32m[HeartBeat] exit\x1b[0m\n");
}

pub fn sched() {
    unsafe {
        KERNEL = Kernel::new();
    }
    task_create("Worker1", worker_task, 1, 2);
    task_create("Worker2", worker_task, 2, 3);
    task_create("Worker3", worker_task, 3, 2);
    task_create("HeartBeat", heartbeat_task, 4, 4);
    schedule();
    puts("sched done\n");
}
