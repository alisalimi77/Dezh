//! One byte in, both devices out — and the number formatting a `no_std` kernel
//! has to write for itself.

use crate::dev::{uart, vga};

pub(crate) fn init() {
    uart::init();
    vga::clear();
}

pub(crate) fn putb(b: u8) {
    if b == b'\n' {
        uart::putb(b'\r');
    }
    uart::putb(b);
    vga::putb(b);
}

pub(crate) fn print(s: &str) {
    for b in s.bytes() {
        putb(b);
    }
}

pub(crate) fn print_i64(mut v: i64) {
    if v < 0 {
        putb(b'-');
        v = -v;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for &b in &buf[i..] {
        putb(b);
    }
}

pub(crate) fn print_hex(mut v: u64) {
    print("0x");
    let mut buf = [0u8; 16];
    for i in (0..16).rev() {
        let nib = (v & 0xF) as u8;
        buf[i] = if nib < 10 { b'0' + nib } else { b'a' + nib - 10 };
        v >>= 4;
    }
    for &b in &buf {
        putb(b);
    }
}
