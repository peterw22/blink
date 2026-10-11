//! `blink PROG ARGS...` — same command line as C blink, driven from Rust.
//! On Linux, BLINK_INPROCESS=1 runs guest processes as threads of this
//! process (as on the Xbox) instead of forking. On Windows that's the only
//! way; the guest's environment is a Linux default plus any host variables
//! named GUEST_<NAME>, which are passed on as <NAME>.

#[cfg(unix)]
fn main() {
    use std::os::unix::ffi::OsStringExt;
    let args = std::env::args_os().map(|a| a.into_vec());
    let in_process = std::env::var_os("BLINK_INPROCESS").is_some_and(|v| v == "1");
    std::process::exit(if in_process { blink_xbox::run_in_process(args) } else { blink_xbox::run(args) });
}

#[cfg(windows)]
fn main() {
    let mut env: Vec<(String, String)> = [
        ("HOME", "/root"),
        ("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"),
        ("TERM", "xterm-256color"),
        ("LANG", "C.UTF-8"),
        ("USER", "root"),
        ("SHELL", "/bin/bash"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for (k, v) in std::env::vars() {
        if let Some(name) = k.strip_prefix("GUEST_") {
            env.retain(|(e, _)| e != name);
            env.push((name.to_string(), v));
        }
    }
    let env = env.into_iter().map(|(k, v)| format!("{k}={v}"));
    std::process::exit(blink_xbox::run(std::env::args(), env));
}
