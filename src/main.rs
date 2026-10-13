#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

mod block;

mod fat12;
use fat12::Fat12;

mod virtio;
use crate::virtio::VirtioBlock;

mod shell;
use shell::puts;

global_asm!(
    r#"
    .section .text.entry
    .global _start

_start:
    la sp, __stack_top

    call kernel_main

1:
    wfi
    j 1b
"#
);

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    let base = match virtio::find_block_device() {
        Some(base) => base,
        None => {
            puts("VirtIO block not found!\n");
            halt();
        }
    };

    if virtio::test_m3(base) {
        puts("[M3] OK\n");
    } else {
        puts("[M3] FAILED\n");
        halt();
    }

    let mut disk = match VirtioBlock::new(base) {
        Some(disk) => disk,
        None => {
            puts("VirtIO init failed!\n");
            halt();
        }
    };

    let mut fs = Fat12::new(&mut disk);

    shell::run(&mut fs);

    halt();
}

fn halt() -> ! {
    loop {
        unsafe {
            asm!("wfi");
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    puts("\nKernel panic!\n");
    halt();
}
