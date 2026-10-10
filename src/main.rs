//! `blink PROG ARGS...` — same command line as C blink, driven from Rust.
use std::os::unix::ffi::OsStringExt;

fn main() {
    let args = std::env::args_os().map(|a| a.into_vec());
    std::process::exit(blink_xbox::run(args));
}
