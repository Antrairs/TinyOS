use core::arch::asm;
use core::ptr::{copy_nonoverlapping, read_volatile, write_volatile};

use crate::block::{BlockDevice, SECTOR_SIZE};

const VIRTIO_BASE: usize = 0x1000_1000;
const VIRTIO_SIZE: usize = 0x1000;
const VIRTIO_COUNT: usize = 8;
const QUEUE_SIZE: usize = 8;
const POLL_LIMIT: usize = 10_000_000;

const STATUS: usize = 0x070;
const ACKNOWLEDGE: u32 = 1;
const DRIVER: u32 = 2;
const DRIVER_OK: u32 = 4;
const FEATURES_OK: u32 = 8;
const DEVICE_ERROR: u32 = 64 | 128;
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

#[repr(C)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
struct Avail {
    flags: u16,
    idx: u16,
    ring: [u16; QUEUE_SIZE],
}

#[repr(C)]
struct UsedEntry {
    id: u32,
    len: u32,
}

#[repr(C)]
struct Used {
    flags: u16,
    idx: u16,
    ring: [UsedEntry; QUEUE_SIZE],
}

#[repr(C, align(16))]
struct Queue {
    desc: [Desc; QUEUE_SIZE],
    avail: Avail,
    used: Used,
}

#[repr(C)]
struct Request {
    kind: u32, // 0 = 读磁盘 1 = 写磁盘
    reserved: u32,
    sector: u64,
}

static mut QUEUE: Queue = Queue {
    desc: [const {
        Desc {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        }
    }; QUEUE_SIZE],
    avail: Avail {
        flags: 1,
        idx: 0,
        ring: [0; QUEUE_SIZE],
    }, // 轮询
    used: Used {
        flags: 0,
        idx: 0,
        ring: [const { UsedEntry { id: 0, len: 0 } }; QUEUE_SIZE],
    },
};
static mut REQUEST: Request = Request {
    kind: 0,
    reserved: 0,
    sector: 0,
};
static mut DATA: [u8; SECTOR_SIZE] = [0; SECTOR_SIZE];
static mut RESULT: u8 = 0xff;

pub struct VirtioBlock {
    base: usize,
}

impl VirtioBlock {
    pub fn new(base: usize) -> Option<Self> {
        // 固定共享内存 只支持当前单核 QEMU 的一个磁盘实例 支持并发或地址映射时需重做内存管理
        if !is_block_device(base) {
            return None;
        }

        write_reg(base, STATUS, 0);
        for _ in 0..POLL_LIMIT {
            if read_reg(base, STATUS) == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if read_reg(base, STATUS) != 0 {
            return None;
        }

        // 队列在静态内存中 热启动后不会清零 设备复位后索引从 0 开始，必须同步复位
        unsafe {
            write_volatile(&raw mut QUEUE.avail.flags, 1);
            write_volatile(&raw mut QUEUE.avail.idx, 0);
            write_volatile(&raw mut QUEUE.used.flags, 0);
            write_volatile(&raw mut QUEUE.used.idx, 0);
        }

        write_reg(base, STATUS, ACKNOWLEDGE);
        write_reg(base, STATUS, ACKNOWLEDGE | DRIVER);

        // VERSION_1 是功能位 32 即第 1 组的第 0 位 其他可选功能不启用
        write_reg(base, 0x014, 1);
        if read_reg(base, 0x010) & 1 == 0 {
            return None;
        }
        write_reg(base, 0x024, 0);
        write_reg(base, 0x020, 0);
        write_reg(base, 0x024, 1);
        write_reg(base, 0x020, 1);
        write_reg(base, STATUS, ACKNOWLEDGE | DRIVER | FEATURES_OK);
        let status = read_reg(base, STATUS);
        if status & FEATURES_OK == 0 || status & DEVICE_ERROR != 0 {
            return None;
        }

        write_reg(base, 0x030, 0); // 选择队列 0
        if read_reg(base, 0x044) != 0 || read_reg(base, 0x034) < QUEUE_SIZE as u32 {
            return None;
        }
        write_reg(base, 0x038, QUEUE_SIZE as u32);
        write_addr(base, 0x080, unsafe { (&raw const QUEUE.desc) as u64 });
        write_addr(base, 0x090, unsafe { (&raw const QUEUE.avail) as u64 });
        write_addr(base, 0x0a0, unsafe { (&raw const QUEUE.used) as u64 });
        write_reg(base, 0x044, 1);
        if read_reg(base, 0x044) != 1 {
            return None;
        }
        let ready = ACKNOWLEDGE | DRIVER | FEATURES_OK | DRIVER_OK;
        write_reg(base, STATUS, ready);
        let status = read_reg(base, STATUS);
        if status & ready != ready || status & DEVICE_ERROR != 0 {
            return None;
        }
        Some(Self { base })
    }

    fn request(&mut self, sector: usize, write: bool) {
        unsafe {
            write_volatile(
                &raw mut REQUEST,
                Request {
                    kind: if write { 1 } else { 0 },
                    reserved: 0,
                    sector: sector as u64,
                },
            );
            write_volatile(&raw mut RESULT, 0xff);

            // 每次只提交一个请求 固定复用 0 → 1 → 2 三个描述符
            write_volatile(
                &raw mut QUEUE.desc[0],
                Desc {
                    addr: (&raw const REQUEST) as u64,
                    len: 16,
                    flags: DESC_NEXT,
                    next: 1,
                },
            );
            write_volatile(
                &raw mut QUEUE.desc[1],
                Desc {
                    addr: (&raw const DATA) as u64,
                    len: SECTOR_SIZE as u32,
                    flags: DESC_NEXT | if write { 0 } else { DESC_WRITE },
                    next: 2,
                },
            );
            write_volatile(
                &raw mut QUEUE.desc[2],
                Desc {
                    addr: (&raw const RESULT) as u64,
                    len: 1,
                    flags: DESC_WRITE,
                    next: 0,
                },
            );

            let avail_idx = read_volatile(&raw const QUEUE.avail.idx);
            let used_idx = read_volatile(&raw const QUEUE.used.idx);
            let slot = avail_idx as usize % QUEUE_SIZE;
            write_volatile(&raw mut QUEUE.avail.ring[slot], 0);
            fence(); // 请求内容必须先于索引和通知对设备可见
            write_volatile(&raw mut QUEUE.avail.idx, avail_idx.wrapping_add(1));
            fence();
            write_reg(self.base, 0x050, 0); // 通知设备处理队列 0

            for _ in 0..POLL_LIMIT {
                if read_volatile(&raw const QUEUE.used.idx) == used_idx.wrapping_add(1) {
                    fence(); // 先观察完成标记，再读取设备写入的结果
                    let slot = used_idx as usize % QUEUE_SIZE;
                    assert_eq!(read_volatile(&raw const QUEUE.used.ring[slot].id), 0);
                    assert_eq!(read_volatile(&raw const RESULT), 0, "VirtIO request failed");
                    return;
                }
                core::hint::spin_loop();
            }
            panic!("VirtIO request timed out");
        }
    }
}

impl BlockDevice for VirtioBlock {
    fn read_sector(&mut self, sector: usize, buf: &mut [u8; SECTOR_SIZE]) {
        self.request(sector, false);
        unsafe {
            copy_nonoverlapping(
                (&raw const DATA).cast::<u8>(),
                buf.as_mut_ptr(),
                SECTOR_SIZE,
            );
        }
    }

    fn write_sector(&mut self, sector: usize, buf: &[u8; SECTOR_SIZE]) {
        unsafe {
            copy_nonoverlapping(buf.as_ptr(), (&raw mut DATA).cast::<u8>(), SECTOR_SIZE);
        }
        self.request(sector, true);
    }
}

fn fence() {
    unsafe {
        asm!("fence iorw, iorw", options(nostack));
    }
}

fn read_reg(base: usize, offset: usize) -> u32 {
    unsafe { read_volatile((base + offset) as *const u32) }
}

fn write_reg(base: usize, offset: usize, value: u32) {
    unsafe {
        write_volatile((base + offset) as *mut u32, value);
    }
    fence();
}

fn write_addr(base: usize, offset: usize, addr: u64) {
    write_reg(base, offset, addr as u32);
    write_reg(base, offset + 4, (addr >> 32) as u32);
}

fn is_block_device(base: usize) -> bool {
    read_reg(base, 0x000) == 0x7472_6976 && read_reg(base, 0x004) == 2 && read_reg(base, 0x008) == 2
}

pub fn find_block_device() -> Option<usize> {
    for i in 0..VIRTIO_COUNT {
        let base = VIRTIO_BASE + i * VIRTIO_SIZE;
        if is_block_device(base) {
            return Some(base);
        }
    }
    None
}
