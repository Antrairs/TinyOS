use core::arch::asm;
use core::ptr::{NonNull, addr_of_mut, read_volatile, write_volatile};
use core::str::from_utf8;

use crate::block::{BlockDevice, SECTOR_SIZE};
use crate::shell::puts;

use virtio_drivers::{
    BufferDirection, Hal, PAGE_SIZE, PhysAddr,
    device::blk::VirtIOBlk,
    transport::{
        DeviceType, Transport,
        mmio::{MmioTransport, VirtIOHeader},
    },
};

const VIRTIO_BASE: usize = 0x1000_1000;
const VIRTIO_SIZE: usize = 0x1000;
const VIRTIO_COUNT: usize = 8;

const MAGIC_VALUE: usize = 0x000;
const VERSION: usize = 0x004;
const DEVICE_ID: usize = 0x008;
const VENDOR_ID: usize = 0x00C;

const DEVICE_FEATURES: usize = 0x010;
const DEVICE_FEATURES_SEL: usize = 0x014;

const DRIVER_FEATURES: usize = 0x020;
const DRIVER_FEATURES_SEL: usize = 0x024;

const STATUS: usize = 0x070;

const STATUS_ACKNOWLEDGE: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;
const STATUS_NEEDS_RESET: u32 = 64;
const STATUS_FAILED: u32 = 128;

// 队列最多使用 8 个 Descriptor
const QUEUE_SIZE: usize = 8;

// 三个区域有不同的对齐要求
#[repr(align(16))]
struct DescMemory([u8; 128]);

#[repr(align(2))]
struct AvailMemory([u8; 22]);

#[repr(align(4))]
struct UsedMemory([u8; 70]);

static mut DESC_MEM: DescMemory = DescMemory([0; 128]);
static mut AVAIL_MEM: AvailMemory = AvailMemory([0; 22]);
static mut USED_MEM: UsedMemory = UsedMemory([0; 70]);

// VirtIO 描述符：16 字节
#[repr(C)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

// 磁盘请求头：16 字节
#[repr(C)]
struct BlkReq {
    kind: u32, // 0 = 读取
    reserved: u32,
    sector: u64,
}

#[repr(align(16))]
struct DataMemory([u8; 512]);

static mut READ_REQ: BlkReq = BlkReq {
    kind: 0,
    reserved: 0,
    sector: 0,
};

static mut READ_DATA: DataMemory = DataMemory([0; 512]);
static mut READ_STATUS: u8 = 0xff;

// 将 64 位物理地址写入两个 32 位 MMIO 寄存器
fn write_addr(base: usize, offset: usize, addr: u64) {
    write_reg(base, offset, addr as u32);
    write_reg(base, offset + 4, (addr >> 32) as u32);
}

fn setup_queue(base: usize) -> bool {
    // 1. 选择 Queue 0
    write_reg(base, 0x030, 0);

    // 2. 检查队列是否可用
    if read_reg(base, 0x044) != 0 {
        return false;
    }
    if read_reg(base, 0x034) < QUEUE_SIZE as u32 {
        return false;
    }

    // 3. 取得三块静态内存的地址
    let desc = core::ptr::addr_of_mut!(DESC_MEM) as u64;
    let avail = core::ptr::addr_of_mut!(AVAIL_MEM) as u64;
    let used = core::ptr::addr_of_mut!(USED_MEM) as u64;

    // 4. 设置队列容量及物理地址
    write_reg(base, 0x038, QUEUE_SIZE as u32);

    write_addr(base, 0x080, desc);
    write_addr(base, 0x090, avail);
    write_addr(base, 0x0A0, used);

    // 5. 启用队列
    write_reg(base, 0x044, 1);

    read_reg(base, 0x044) == 1
}

fn test_read_sector0(base: usize) -> bool {
    let desc = addr_of_mut!(DESC_MEM) as *mut Desc;
    let avail = addr_of_mut!(AVAIL_MEM) as *mut u8;
    let used = addr_of_mut!(USED_MEM) as *mut u8;

    let req_addr = addr_of_mut!(READ_REQ) as u64;
    let data_addr = addr_of_mut!(READ_DATA) as u64;
    let status_addr = addr_of_mut!(READ_STATUS) as u64;

    unsafe {
        // 1. 准备读取请求
        write_volatile(
            addr_of_mut!(READ_REQ),
            BlkReq {
                kind: 0, // VIRTIO_BLK_T_IN
                reserved: 0,
                sector: 0,
            },
        );
        write_volatile(addr_of_mut!(READ_STATUS), 0xff);

        // 2. 三个描述符组成 0 -> 1 -> 2
        write_volatile(
            desc.add(0),
            Desc {
                addr: req_addr,
                len: 16,
                flags: 1, // NEXT
                next: 1,
            },
        );

        write_volatile(
            desc.add(1),
            Desc {
                addr: data_addr,
                len: 512,
                flags: 3, // NEXT | WRITE
                next: 2,
            },
        );

        write_volatile(
            desc.add(2),
            Desc {
                addr: status_addr,
                len: 1,
                flags: 2, // WRITE
                next: 0,
            },
        );

        // 3. 记录当前队列索引
        let avail_idx = read_volatile(avail.add(2) as *const u16);
        let used_idx = read_volatile(used.add(2) as *const u16);

        // 将描述符链的起点 0 放进 Available Ring
        let slot = (avail_idx as usize) % QUEUE_SIZE;
        write_volatile(avail.add(4 + slot * 2) as *mut u16, 0);

        // 先发布描述符，再更新 avail.idx
        asm!("fence iorw, iorw", options(nostack));
        write_volatile(avail.add(2) as *mut u16, avail_idx.wrapping_add(1));

        // 4. 通知设备：Queue 0 有新请求
        asm!("fence iorw, iorw", options(nostack));
        write_reg(base, 0x050, 0);

        // 5. 等待设备完成
        let expected = used_idx.wrapping_add(1);
        let mut completed = false;

        for _ in 0..10_000_000 {
            if read_volatile(used.add(2) as *const u16) == expected {
                completed = true;
                break;
            }
            core::hint::spin_loop();
        }

        if !completed {
            puts("[M4] Read timeout\n");
            return false;
        }

        // 确保设备写入的结果对 CPU 可见
        asm!("fence iorw, iorw", options(nostack));

        // 确认完成的是 Descriptor 0 发起的请求
        let used_slot = (used_idx as usize) % QUEUE_SIZE;
        let completed_id = read_volatile(used.add(4 + used_slot * 8) as *const u32);

        let status = read_volatile(addr_of_mut!(READ_STATUS));

        if completed_id != 0 || status != 0 {
            puts("[M4] Read failed\n");
            return false;
        }

        puts("[M4] Sector 0 read OK\n");

        // 6. 检查 FAT12 Boot Sector 签名
        let data = addr_of_mut!(READ_DATA) as *const u8;
        let lo = read_volatile(data.add(510));
        let hi = read_volatile(data.add(511));

        if lo == 0x55 && hi == 0xAA {
            puts("[M4] Boot signature 55 AA found\n");
        } else {
            puts("[M4] No 55 AA signature (disk may be unformatted)\n");
        }
    }

    true
}

fn print_hex(value: u32) {
    let digits = b"0123456789ABCDEF";
    let mut buf = *b"0x00000000";

    for i in 0..8 {
        let shift = 4 * (7 - i);
        buf[i + 2] = digits[((value >> shift) & 0xF) as usize];
    }
    puts(from_utf8(&buf).unwrap());
}

pub fn test_m4(base: usize) -> bool {
    const STATUS: usize = 0x070;

    // 1. 复位并声明自己是驱动
    write_reg(base, STATUS, 0);
    while read_reg(base, STATUS) != 0 {}

    write_reg(base, STATUS, 1); // ACKNOWLEDGE
    write_reg(base, STATUS, 3); // ACKNOWLEDGE | DRIVER

    // 2. 检查设备是否支持现代 VirtIO（Feature 32）
    write_reg(base, 0x014, 1);
    if read_reg(base, 0x010) & 1 == 0 {
        write_reg(base, STATUS, 0);
        return false;
    }

    // 只启用 VERSION_1，其他可选功能都不启用
    write_reg(base, 0x024, 0);
    write_reg(base, 0x020, 0);

    write_reg(base, 0x024, 1);
    write_reg(base, 0x020, 1);

    // 3. 检查设备是否接受
    write_reg(base, STATUS, 11); // 1 | 2 | 8

    let status = read_reg(base, STATUS);

    if status & 8 == 0 || status & (64 | 128) != 0 {
        write_reg(base, STATUS, 0);
        return false;
    }

    // M3：配置 Virtqueue
    let queue_ok = setup_queue(base);

    // 队列配置完成后，才能设置 DRIVER_OK
    if queue_ok {
        write_reg(base, STATUS, 15);
    }

    let status = read_reg(base, STATUS);
    let ok = queue_ok && status & 15 == 15 && status & (64 | 128) == 0;

    // M4：队列已经准备好，尝试读取 Sector 0
    let read_ok = ok && test_read_sector0(base);

    // 测试结束后复位，让原来的驱动重新初始化
    write_reg(base, STATUS, 0);

    read_ok && read_reg(base, STATUS) == 0
}

pub struct TinyHal;

const DMA_PAGES: usize = 16;

#[repr(align(4096))]
struct DmaPool([u8; DMA_PAGES * PAGE_SIZE]);

static mut DMA_POOL: DmaPool = DmaPool([0; DMA_PAGES * PAGE_SIZE]);

static mut DMA_NEXT: usize = 0;

unsafe impl Hal for TinyHal {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let bytes = pages * PAGE_SIZE;

        unsafe {
            if DMA_NEXT + bytes > DMA_PAGES * PAGE_SIZE {
                return (0, NonNull::dangling());
            }

            let base = core::ptr::addr_of_mut!(DMA_POOL.0) as *mut u8;

            let ptr = base.add(DMA_NEXT);

            DMA_NEXT += bytes;

            core::ptr::write_bytes(ptr, 0, bytes);

            (ptr as usize as u64, NonNull::new_unchecked(ptr))
        }
    }

    unsafe fn dma_dealloc(_paddr: PhysAddr, _vaddr: NonNull<u8>, _pages: usize) -> i32 {
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        unsafe { NonNull::new_unchecked(paddr as usize as *mut u8) }
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        buffer.as_ptr() as *mut u8 as usize as u64
    }

    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {}
}

pub struct VirtioBlock {
    inner: VirtIOBlk<TinyHal, MmioTransport<'static>>,
}

impl VirtioBlock {
    pub fn new(base: usize) -> Option<Self> {
        let header = NonNull::new(base as *mut VirtIOHeader)?;

        let transport = unsafe { MmioTransport::new(header, VIRTIO_SIZE).ok()? };

        if transport.device_type() != DeviceType::Block {
            return None;
        }

        let inner = VirtIOBlk::<TinyHal, _>::new(transport).ok()?;

        Some(Self { inner })
    }
}

impl BlockDevice for VirtioBlock {
    fn read_sector(&mut self, sector: usize, buf: &mut [u8; SECTOR_SIZE]) {
        self.inner
            .read_blocks(sector, buf)
            .expect("VirtIO read failed");
    }

    fn write_sector(&mut self, sector: usize, buf: &[u8; SECTOR_SIZE]) {
        self.inner
            .write_blocks(sector, buf)
            .expect("VirtIO write failed");
    }
}

fn read_reg(base: usize, offset: usize) -> u32 {
    unsafe { read_volatile((base + offset) as *const u32) }
}

fn write_reg(base: usize, offset: usize, value: u32) {
    unsafe {
        write_volatile((base + offset) as *mut u32, value);
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
}

fn is_block_device(base: usize) -> bool {
    let magic = read_reg(base, MAGIC_VALUE);
    let version = read_reg(base, VERSION);
    let device_id = read_reg(base, DEVICE_ID);

    magic == 0x7472_6976 && version == 2 && device_id == 2
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
