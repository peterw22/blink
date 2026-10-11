//! `blink PROG ARGS...` — same command line as C blink, driven from Rust.
//! With BLINK_INPROCESS=1, guest processes run as threads of this process
//! (as on the Xbox) instead of being forked.
use std::os::unix::ffi::OsStringExt;

fn main() {
    let args = std::env::args_os().map(|a| a.into_vec());
    let in_process = std::env::var_os("BLINK_INPROCESS").is_some_and(|v| v == "1");
    std::process::exit(if in_process { blink_xbox::run_in_process(args) } else { blink_xbox::run(args) });
}
