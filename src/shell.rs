use core::ptr::{read_volatile, write_volatile};

use crate::fat12::{Fat12, to_83};

const RESET: &str = "\x1b[0m";
const CYAN: &str = "\x1b[36m";
const BLUE: &str = "\x1b[34m";
const GREEN: &str = "\x1b[32m";
const BOLD: &str = "\x1b[1m";
const YELLOW: &str = "\x1b[33m";
const REVERSE: &str = "\x1b[7m";

enum Key {
    Char(u8),
    Enter,
    Up,
    Down,
    Left,
    Right,
}

const UART0: usize = 0x1000_0000;

fn putchar(c: u8) {
    const LSR: usize = 5;
    const TX_EMPTY: u8 = 1 << 5;

    unsafe {
        while read_volatile((UART0 + LSR) as *const u8) & TX_EMPTY == 0 {}
        write_volatile(UART0 as *mut u8, c);
    }
}

pub fn puts(s: &str) {
    for c in s.bytes() {
        putchar(c);
    }
}

fn getchar() -> u8 {
    const LSR: usize = 5;
    const RX_READY: u8 = 1;

    unsafe {
        while read_volatile((UART0 + LSR) as *const u8) & RX_READY == 0 {}
        read_volatile(UART0 as *const u8)
    }
}

fn try_getchar() -> Option<u8> {
    const LSR: usize = 5;
    const RX_READY: u8 = 1;

    unsafe {
        if read_volatile((UART0 + LSR) as *const u8) & RX_READY == 0 {
            None
        } else {
            Some(read_volatile(UART0 as *const u8))
        }
    }
}

fn read_line(buf: &mut [u8]) -> usize {
    let mut len = 0;

    loop {
        let c = getchar();

        match c {
            b'\r' => {
                putchar(b'\n');
                return len;
            }
            127 => {
                if len > 0 {
                    len -= 1;
                    puts("\x08 \x08"); // 光标左移 擦掉字符 再左移
                }
            }
            _ => {
                // 可显示字符范围
                if c >= 32 && c <= 126 && len < buf.len() {
                    buf[len] = c;
                    len += 1;
                    putchar(c);
                }
            }
        }
    }
}

fn enter_fullscreen() {
    puts("\x1b[?1049h"); // alternate screen
    puts("\x1b[2J"); // 清屏
    puts("\x1b[H"); // 左上角
    puts("\x1b[?25l"); // 隐藏光标
}

fn leave_fullscreen() {
    puts(RESET);
    puts("\x1b[?25h"); // 恢复光标
    puts("\x1b[?1049l"); // 回原来的 Shell
}

fn read_key() -> Key {
    let c = getchar();
    match c {
        b'\r' => Key::Enter,
        27 => {
            let second = getchar();
            if second == b'[' {
                let third = getchar();
                match third {
                    b'A' => return Key::Up,
                    b'B' => return Key::Down,
                    b'C' => return Key::Right,
                    b'D' => return Key::Left,
                    _ => Key::Char(0),
                }
            } else {
                Key::Char(0)
            }
        }
        _ => Key::Char(c),
    }
}

#[derive(Clone, Copy)]
struct BrowserEntry {
    name: [u8; 11],
    attr: u8,
    cluster: u16,
    size: u32,
}

fn print_name(name: &[u8; 11]) {
    let mut base_end = 8;

    while base_end > 0 && name[base_end - 1] == b' ' {
        base_end -= 1;
    }

    for &c in &name[..base_end] {
        putchar(c);
    }

    let mut ext_end = 11;

    while ext_end > 8 && name[ext_end - 1] == b' ' {
        ext_end -= 1;
    }

    if ext_end > 8 {
        putchar(b'.');
        for &c in &name[8..ext_end] {
            putchar(c);
        }
    }
}

fn print_number(mut value: u32) -> usize {
    let mut digits = [0u8; 10];
    let mut start = digits.len();

    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;

        if value == 0 {
            break;
        }
    }

    for &digit in &digits[start..] {
        putchar(digit);
    }

    digits.len() - start
}

fn centered_row(text: &str, width: usize, title: bool) {
    let inner = width.saturating_sub(4);
    let left = inner.saturating_sub(text.len()) / 2;
    let right = inner.saturating_sub(text.len() + left);

    row_start();

    for _ in 0..left {
        putchar(b' ');
    }

    if title {
        puts(CYAN);
        puts(BOLD);
    }

    puts(text);
    puts(RESET);

    for _ in 0..right {
        putchar(b' ');
    }

    row_end();
}

fn border_line(left: &str, right: &str, width: usize) {
    puts(RESET);
    puts(CYAN);

    puts(left);
    for _ in 0..width.saturating_sub(2) {
        puts("─");
    }
    puts(right);

    puts(RESET);
}

fn row_start() {
    puts(RESET);
    puts(CYAN);
    puts("│ ");
    puts(RESET);
}

fn row_end() {
    puts(RESET);
    puts(CYAN);
    puts(" │");
    puts(RESET);
    puts("\n");
}

fn draw_file(entry: &BrowserEntry, data: Option<&[u8]>, size: TermSize) {
    puts(RESET);
    puts("\x1b[H\x1b[2J");

    if size.cols < 40 || size.rows < 10 {
        puts("Terminal too small. Left / q: back");
        return;
    }

    let width = size.cols;
    let inner = width - 4;
    let content_rows = size.rows - 8;

    puts(CYAN);
    border_line("╭", "╮", width);
    puts("\n");
    centered_row("TinyOS File Viewer", width, true);
    border_line("├", "┤", width);
    puts("\r\n");

    row_start();
    puts("File: ");
    print_name(&entry.name);

    let mut base_end = 8;
    while base_end > 0 && entry.name[base_end - 1] == b' ' {
        base_end -= 1;
    }

    let mut ext_end = 11;
    while ext_end > 8 && entry.name[ext_end - 1] == b' ' {
        ext_end -= 1;
    }

    let name_len = base_end + if ext_end > 8 { 1 + ext_end - 8 } else { 0 };

    for _ in "File: ".len() + name_len..inner {
        putchar(b' ');
    }
    row_end();

    border_line("├", "┤", width);
    puts("\n");
    puts(RESET);

    let message = b"Read failed, or file exceeds 4096 bytes.";
    let bytes = match data {
        Some(bytes) if bytes.is_empty() => &b"(Empty file)"[..],
        Some(bytes) => bytes,
        None => &message[..],
    };

    let mut pos = 0;

    for _ in 0..content_rows {
        row_start();
        let mut col = 0;

        while pos < bytes.len() && col < inner {
            let c = bytes[pos];
            pos += 1;

            if c == b'\r' {
                continue;
            }
            if c == b'\n' {
                break;
            }

            putchar(if c >= 32 && c <= 126 { c } else { b'?' });
            col += 1;
        }

        if col == inner {
            if pos < bytes.len() && bytes[pos] == b'\r' {
                pos += 1;
            }
            if pos < bytes.len() && bytes[pos] == b'\n' {
                pos += 1;
            }
        }

        for _ in col..inner {
            putchar(b' ');
        }
        row_end();
    }

    puts(CYAN);
    border_line("├", "┤", width);
    puts("\n");

    let hint = if pos < bytes.len() {
        "Left/q: back | More content omitted"
    } else {
        "Left / q: return to browser"
    };
    centered_row(hint, width, false);

    border_line("╰", "╯", width);
    puts(RESET);
}

fn draw_browser(entries: &[BrowserEntry], selected: usize, scroll: usize, size: TermSize) {
    if size.cols < 40 || size.rows < 10 {
        puts("\x1b[H\x1b[2J");

        puts(YELLOW);
        puts("TinyOS File Browser\n");
        puts(RESET);

        puts("Terminal too small.\n");
        puts("Resize to at least 40x10.\n");
        puts("\nPress q to quit, r to refresh.\n");

        return;
    }

    let visible = size.rows.saturating_sub(8);
    let width = size.cols;
    puts("\x1b[H");

    puts(CYAN);
    puts(BOLD);
    border_line("╭", "╮", width);
    puts("\n");

    centered_row("TinyOS File Browser", width, true);
    puts(BOLD);
    puts(RESET);
    puts(CYAN);

    border_line("├", "┤", width);
    puts("\n");

    puts(RESET);

    let header = "  TYPE    NAME          SIZE";
    row_start();
    puts(header);
    for _ in header.len()..width.saturating_sub(4) {
        putchar(b' ');
    }
    row_end();

    border_line("├", "┤", width);
    puts("\n");

    for row in 0..visible {
        let index = scroll + row;

        puts(RESET);
        row_start();

        if index >= entries.len() {
            let message = if entries.is_empty() && row == 0 {
                "(Empty directory)"
            } else {
                ""
            };

            puts(message);

            for _ in message.len()..width.saturating_sub(4) {
                putchar(b' ');
            }
            row_end();
            continue;
        }

        let entry = &entries[index];

        if index == selected {
            puts(REVERSE);
        }

        if entry.attr & 0x10 != 0 {
            puts(BLUE);
            puts("> [ DIR]  ");
        } else {
            puts(GREEN);
            puts("  [FILE]  ");
        }

        print_name(&entry.name);

        // 计算实际显示的文件名长度 排除补齐空格
        let mut base_end = 8;
        while base_end > 0 && entry.name[base_end - 1] == b' ' {
            base_end -= 1;
        }

        let mut ext_end = 11;
        while ext_end > 8 && entry.name[ext_end - 1] == b' ' {
            ext_end -= 1;
        }

        let name_len = base_end
            + if ext_end > 8 {
                1 + ext_end - 8 // 点号 + 扩展名
            } else {
                0
            };

        // 文件名补齐到 14 列 让 SIZE 始终从同一列开始
        for _ in name_len..14 {
            putchar(b' ');
        }

        let size_len = if entry.attr & 0x10 != 0 {
            // 目录不显示文件大小
            putchar(b'-');
            1
        } else {
            let digits = print_number(entry.size);
            puts(" B");
            digits + 2
        };

        // 类型区 10 列 名称区 14 列 再加大小文字
        let used = 10 + 14 + size_len;

        for _ in used..width.saturating_sub(4) {
            putchar(b' ');
        }
        // 先关闭选中高亮 避免边框也被反色
        puts(RESET);
        row_end();
    }

    border_line("├", "┤", width);
    puts("\n");

    centered_row(
        "Up/Down Select   Enter/Right Open   Left Back   Q Quit",
        width,
        false,
    );

    border_line("╰", "╯", width);
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TermSize {
    rows: usize,
    cols: usize,
}

fn terminal_size() -> TermSize {
    let default = TermSize { rows: 24, cols: 80 };

    puts("\x1b[18t");

    let mut buf = [0u8; 32];
    let mut len = 0usize;

    for _ in 0..1_000_000 {
        if let Some(c) = try_getchar() {
            if len < buf.len() {
                buf[len] = c;
                len += 1;
            }
            if c == b't' {
                break;
            }
        }
    }

    if len < 7
        || buf[0] != 27
        || buf[1] != b'['
        || buf[2] != b'8'
        || buf[3] != b';'
        || buf[len - 1] != b't'
    {
        return default;
    }

    let mut i = 4;
    let mut rows = 0usize;

    while i < len && buf[i].is_ascii_digit() {
        rows = rows * 10 + (buf[i] - b'0') as usize;
        i += 1;
    }

    if i >= len || buf[i] != b';' {
        return default;
    }

    i += 1;

    let mut cols = 0usize;

    while i < len && buf[i].is_ascii_digit() {
        cols = cols * 10 + (buf[i] - b'0') as usize;
        i += 1;
    }

    if rows < 5 || cols < 20 {
        return default;
    }

    TermSize { rows, cols }
}

fn view_file(fs: &mut Fat12<'_>, current_dir: u16, entry: &BrowserEntry, size: TermSize) {
    let mut buf = [0u8; 4096];
    let len = fs.read_bytes(current_dir, &entry.name, &mut buf);

    let data = match len {
        Some(len) => Some(&buf[..len]),
        None => None,
    };

    draw_file(entry, data, size);

    loop {
        match read_key() {
            Key::Left | Key::Char(b'q') | Key::Char(b'Q') => break,
            _ => {}
        }
    }

    puts(RESET);
    puts("\x1b[H\x1b[2J");
}

fn browser(fs: &mut Fat12<'_>, start_dir: u16) -> u16 {
    let mut size = terminal_size();
    const MAX_ENTRIES: usize = 224;
    let mut visible = size.rows.saturating_sub(8).max(1);

    let empty = BrowserEntry {
        name: [b' '; 11],
        attr: 0,
        cluster: 0,
        size: 0,
    };

    let mut current_dir = start_dir;
    let mut selected = 0usize;
    let mut scroll = 0usize;

    enter_fullscreen();

    loop {
        let mut entries = [empty; MAX_ENTRIES];
        let mut count = 0usize;

        fs.list_dir(current_dir, |name, attr, cluster, size| {
            // 不显示 "." 和 ".."
            if name == b".          " || name == b"..         " {
                return;
            }

            if count < MAX_ENTRIES {
                entries[count].name.copy_from_slice(name);
                entries[count].attr = attr;
                entries[count].cluster = cluster;
                entries[count].size = size;

                count += 1;
            }
        });

        if count == 0 {
            selected = 0;
            scroll = 0;
        } else if selected >= count {
            selected = count - 1;
        }

        draw_browser(&entries[..count], selected, scroll, size);

        let key = read_key();
        let new_size = terminal_size();

        if new_size != size {
            size = new_size;
            visible = size.rows.saturating_sub(8).max(1);

            // 调整滚动位置 确保选中项仍在显示范围内
            if selected < scroll {
                scroll = selected;
            } else if selected >= scroll + visible {
                scroll = selected + 1 - visible;
            }

            puts("\x1b[H\x1b[2J");
        }

        match key {
            Key::Up => {
                if selected > 0 {
                    selected -= 1;

                    if selected < scroll {
                        scroll = selected;
                    }
                }
            }

            Key::Down => {
                if selected + 1 < count {
                    selected += 1;

                    if selected >= scroll + visible {
                        scroll = selected + 1 - visible;
                    }
                }
            }

            Key::Enter | Key::Right => {
                if count > 0 {
                    let entry = entries[selected];

                    if entry.attr & 0x10 != 0 {
                        current_dir = entry.cluster;

                        selected = 0;
                        scroll = 0;
                    } else if entry.attr & 0x08 == 0 {
                        view_file(fs, current_dir, &entry, size);
                    }
                }
            }

            Key::Left => {
                if current_dir != 0 {
                    if let Some(parent) = fs.find_dir(current_dir, b"..         ") {
                        current_dir = parent;
                        selected = 0;
                        scroll = 0;
                    }
                }
            }

            Key::Char(b'q') | Key::Char(b'Q') => {
                leave_fullscreen();
                return current_dir;
            }

            _ => {}
        }
    }
}

pub fn run(fs: &mut Fat12<'_>) {
    let mut line = [0u8; 128];
    let mut current_dir = 0;

    loop {
        puts("tinyOS> ");

        let len = read_line(&mut line);
        let input = &line[..len];

        if input.is_empty() {
            continue;
        }

        let space = input.iter().position(|&c| c == b' ');

        let (cmd, arg) = match space {
            Some(pos) => {
                let mut arg_start = pos + 1;

                while arg_start < input.len() && input[arg_start] == b' ' {
                    arg_start += 1;
                }
                (&input[..pos], &input[arg_start..])
            }
            None => (input, &[][..]),
        };

        match cmd {
            b"help" => {
                puts("help   - show commands\n");
                puts("sched  - run scheduler (prio|rr)\n");
                puts("browse - open file browser\n");
                puts("dir    - list directory\n");
                puts("touch  - create file\n");
                puts("mkdir  - create directory\n");
                puts("cd     - change directory\n");
                puts("write  - write file\n");
                puts("type   - show file content\n");
                puts("format - format FAT12 disk\n");
            }
            b"sched" => {
                match arg {
                    b"prio" => crate::task::sched(crate::task::SchedPolicy::Prio),
                    b"rr" | b"" => crate::task::sched(crate::task::SchedPolicy::Rr),
                    _ => {
                        puts("Usage: sched [prio|rr]\n");
                    }
                }
            }
            b"browse" => {
                current_dir = browser(fs, current_dir);
            }
            b"dir" => {
                fs.list_dir(current_dir, |name, _, _, _| {
                    for &c in name {
                        putchar(c);
                    }
                    putchar(b'\n');
                });
            }
            b"touch" => {
                if arg.is_empty() {
                    puts("Usage: touch <filename>\n");
                    continue;
                }

                let name = match to_83(arg) {
                    Some(name) => name,
                    None => {
                        puts("Invalid filename\n");
                        continue;
                    }
                };

                if fs.create_file(current_dir, &name) {
                    puts("File created\n");
                } else {
                    puts("Create failed\n");
                }
            }
            b"mkdir" => {
                if arg.is_empty() {
                    puts("Usage: mkdir <dirname>\n");
                    continue;
                }

                let name = match to_83(arg) {
                    Some(name) => name,
                    None => {
                        puts("Invalid directory name\n");
                        continue;
                    }
                };

                if fs.mkdir(current_dir, &name) {
                    puts("Directory created\n");
                } else {
                    puts("Create failed\n");
                }
            }
            b"cd" => {
                if arg.is_empty() {
                    puts("Usage: cd <dirname>\n");
                    continue;
                }

                if arg == b".." {
                    if current_dir != 0 {
                        match fs.find_dir(current_dir, b"..         ") {
                            Some(parent) => {
                                current_dir = parent;
                            }
                            None => {
                                puts("Parent directory not found\n");
                            }
                        }
                    }
                    continue;
                }

                if arg == b"." {
                    continue;
                }

                // cd 普通目录
                let name = match to_83(arg) {
                    Some(name) => name,
                    None => {
                        puts("Invalid directory name\n");
                        continue;
                    }
                };
                match fs.find_dir(current_dir, &name) {
                    Some(cluster) => {
                        current_dir = cluster;
                    }
                    None => {
                        puts("Directory not found\n");
                    }
                }
            }
            b"write" => {
                let space = match arg.iter().position(|&b| b == b' ') {
                    Some(pos) => pos,
                    None => {
                        puts("Usage: write <filename> <text>\n");
                        continue;
                    }
                };

                let filename = &arg[..space];
                let mut text_start = space + 1;

                // 跳过文件名和正文之间多余的空格
                while text_start < arg.len() && arg[text_start] == b' ' {
                    text_start += 1;
                }

                let text = &arg[text_start..];

                let name = match to_83(filename) {
                    Some(name) => name,
                    None => {
                        puts("Invalid filename\n");
                        continue;
                    }
                };

                if fs.write_bytes(current_dir, &name, text) {
                    puts("Write OK\n");
                } else {
                    puts("Write failed\n");
                }
            }
            b"type" => {
                if arg.is_empty() {
                    puts("Usage: type <filename>\n");
                    continue;
                }

                let name = match to_83(arg) {
                    Some(name) => name,
                    None => {
                        puts("Invalid filename\n");
                        continue;
                    }
                };

                let mut buf = [0u8; 4096];
                match fs.read_bytes(current_dir, &name, &mut buf) {
                    Some(size) => {
                        for &c in &buf[..size] {
                            putchar(c);
                        }
                        putchar(b'\n');
                    }
                    None => {
                        puts("Read failed\n");
                    }
                }
            }
            b"format" => {
                fs.format();
                current_dir = 0;
                puts("Format OK\n");
            }
            _ => {
                puts("Unknown command\n");
            }
        }
    }
}
