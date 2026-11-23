use core::arch::{asm, global_asm};
use core::mem::size_of;

use crate::shell::puts;

use crate::trap::{disable_irq, enable_irq, read_time, start_timer, stop_timer, ticks};

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
    Sleeping,
}

#[derive(Copy, Clone)]
struct Tcb {
    sp: usize,
    state: TaskState,
    prio: u8,
    wake_tick: usize,
    name: &'static str,
}

impl Tcb {
    const fn empty() -> Self {
        Self {
            sp: 0,
            state: TaskState::Unused,
            prio: 0,
            wake_tick: 0,
            name: "",
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
pub enum SchedPolicy {
    Prio,
    Rr,
}

struct PrioBitmap {
    rdy_mask: u64,
    prio_tid: [Option<usize>; 64],
}

impl PrioBitmap {
    const fn new() -> Self {
        Self {
            rdy_mask: 0,
            prio_tid: [None; 64],
        }
    }

    fn push(&mut self, tid: usize, prio: u8) {
        let p = prio as usize;
        if p >= 64 {
            return;
        }
        self.rdy_mask |= 1u64 << p;
        self.prio_tid[p] = Some(tid);
    }

    fn remove(&mut self, prio: u8) {
        let p = prio as usize;
        if p >= 64 {
            return;
        }
        self.rdy_mask &= !(1u64 << p);
        self.prio_tid[p] = None;
    }

    fn pop(&mut self) -> Option<usize> {
        if self.rdy_mask == 0 {
            return None;
        }
        let p = self.rdy_mask.trailing_zeros() as usize;
        let tid = self.prio_tid[p];
        self.remove(p as u8);
        tid
    }
}

struct RingQueue {
    buf: [usize; MAX_TASKS],
    head: usize,
    tail: usize,
    len: usize,
}

impl RingQueue {
    const fn new() -> Self {
        Self {
            buf: [0; MAX_TASKS],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    fn push(&mut self, tid: usize) {
        if self.len >= MAX_TASKS {
            return;
        }
        self.buf[self.tail] = tid;
        self.tail = (self.tail + 1) % MAX_TASKS;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        let tid = self.buf[self.head];
        self.head = (self.head + 1) % MAX_TASKS;
        self.len -= 1;
        Some(tid)
    }

    fn remove(&mut self, target: usize) {
        let mut new_len: usize = 0;
        for i in 0..self.len {
            let idx = (self.head + i) % MAX_TASKS;
            let tid = self.buf[idx];
            if tid != target {
                self.buf[(self.head + new_len) % MAX_TASKS] = tid;
                new_len += 1;
            }
        }
        self.tail = (self.head + new_len) % MAX_TASKS;
        self.len = new_len;
    }
}

struct Kernel {
    tcbs: [Tcb; MAX_TASKS],
    used_mask: u32,
    current: Option<usize>,
    shell_sp: usize,
    policy: SchedPolicy,
    prio_q: PrioBitmap,
    rr_q: RingQueue,
}

impl Kernel {
    const fn new(policy: SchedPolicy) -> Self {
        Self {
            tcbs: [Tcb::empty(); MAX_TASKS],
            used_mask: 0,
            current: None,
            shell_sp: 0,
            policy,
            prio_q: PrioBitmap::new(),
            rr_q: RingQueue::new(),
        }
    }
}

static mut KERNEL: Kernel = Kernel::new(SchedPolicy::Rr);
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
    csrsi sstatus, 2
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

fn ready_push(tid: usize, prio: u8) {
    unsafe {
        match KERNEL.policy {
            SchedPolicy::Prio => (*(&raw mut KERNEL.prio_q)).push(tid, prio),
            SchedPolicy::Rr => (*(&raw mut KERNEL.rr_q)).push(tid),
        }
    }
}

fn ready_pop() -> Option<usize> {
    unsafe {
        match KERNEL.policy {
            SchedPolicy::Prio => (*(&raw mut KERNEL.prio_q)).pop(),
            SchedPolicy::Rr => (*(&raw mut KERNEL.rr_q)).pop(),
        }
    }
}

fn ready_remove(tid: usize, prio: u8) {
    unsafe {
        match KERNEL.policy {
            SchedPolicy::Prio => (*(&raw mut KERNEL.prio_q)).remove(prio),
            SchedPolicy::Rr => (*(&raw mut KERNEL.rr_q)).remove(tid),
        }
    }
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
            wake_tick: 0,
            name,
        };
        ready_push(tid, prio);
        Some(tid)
    }
}

fn task_delete(tid: usize) {
    if tid >= MAX_TASKS {
        return;
    }
    unsafe {
        let state = KERNEL.tcbs[tid].state;
        if state == TaskState::Ready {
            let prio = KERNEL.tcbs[tid].prio;
            ready_remove(tid, prio);
        }

        KERNEL.used_mask &= !(1 << tid);
        KERNEL.tcbs[tid].state = TaskState::Unused;
    }
}

fn schedule() {
    unsafe {
        let prev = KERNEL.current;

        let mut next = ready_pop();
        while next.is_none() && KERNEL.used_mask != 0 {
            KERNEL.current = None;
            enable_irq();
            asm!("wfi");
            disable_irq();
            KERNEL.current = prev;
            next = ready_pop();
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

pub fn on_tick() {
    let now = ticks();
    unsafe {
        for tid in 0..MAX_TASKS {
            if KERNEL.tcbs[tid].state == TaskState::Sleeping && KERNEL.tcbs[tid].wake_tick <= now {
                KERNEL.tcbs[tid].state = TaskState::Ready;
                let prio = KERNEL.tcbs[tid].prio;
                ready_push(tid, prio);
            }
        }
        let Some(cur) = KERNEL.current else {
            return;
        };
        if KERNEL.tcbs[cur].state == TaskState::Running {
            KERNEL.tcbs[cur].state = TaskState::Ready;
            let prio = KERNEL.tcbs[cur].prio;
            ready_push(cur, prio);
        }
    }
    schedule();
}

fn task_yield() {
    disable_irq();
    unsafe {
        if let Some(cur) = KERNEL.current {
            let state = KERNEL.tcbs[cur].state;
            if state == TaskState::Running {
                KERNEL.tcbs[cur].state = TaskState::Ready;
                let prio = KERNEL.tcbs[cur].prio;
                ready_push(cur, prio);
            }
        }
    }
    schedule();
    enable_irq();
}

fn task_sleep(n: usize) {
    if n == 0 {
        task_yield();
        return;
    }
    disable_irq();
    unsafe {
        if let Some(cur) = KERNEL.current {
            KERNEL.tcbs[cur].state = TaskState::Sleeping;
            KERNEL.tcbs[cur].wake_tick = ticks() + n;
        }
    }
    schedule();
    enable_irq();
}

#[unsafe(no_mangle)]
extern "C" fn task_exit() -> ! {
    disable_irq();
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

fn busy_wait(cycles: u64) {
    let end = read_time() + cycles;
    while read_time() < end {}
}

extern "C" fn worker_task(rounds: usize) {
    for _ in 0..rounds {
        puts("[");
        puts(current_name());
        puts("] working\n");
        busy_wait(1_000_000);
    }
    puts("[");
    puts(current_name());
    puts("] exiting\n");
}

extern "C" fn heartbeat_task(beats: usize) {
    for _ in 0..beats {
        puts("\x1b[32m[HeartBeat] alive\x1b[0m\n");
        task_sleep(2);
    }
    puts("\x1b[32m[HeartBeat] exit\x1b[0m\n");
}

pub fn sched(policy: SchedPolicy) {
    unsafe {
        KERNEL = Kernel::new(policy);
    }
    task_create("Worker1", worker_task, 1, 2);
    task_create("Worker2", worker_task, 2, 3);
    task_create("Worker3", worker_task, 3, 2);
    task_create("HeartBeat", heartbeat_task, 0, 4);
    start_timer();
    schedule();
    stop_timer();
    puts("sched done\n");
}
