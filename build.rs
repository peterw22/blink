// Compiles the C blink emulator (../blink, branch run-claude-code) into a static
// library linked into this crate. BLINK_DIR overrides the source location.
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let blink = std::env::var("BLINK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("../blink"));
    let blink = blink.canonicalize().expect("blink sources not found; set BLINK_DIR");
    let target = std::env::var("TARGET").unwrap();
    println!("cargo:rerun-if-env-changed=BLINK_DIR");
    println!("cargo:rerun-if-changed={}", blink.join("blink").display());
    println!("cargo:rerun-if-changed={}", blink.join("config.h").display());

    if !target.contains("linux") {
        // The Windows (UWP) host layer doesn't exist yet: blink's C code still
        // needs POSIX. Build the Rust side only so `cargo check` keeps working.
        println!("cargo:warning=blink C sources are only linked for Linux targets so far");
        return;
    }

    let mut sources = Vec::new();
    for entry in std::fs::read_dir(blink.join("blink")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        if name.ends_with(".c") && name != "blinkenlights.c" {
            sources.push((name, path));
        }
    }
    sources.sort();

    // Flags from blink's config.mk; GCC rather than the user's `cc` (zig) shim,
    // whose bundled glibc 2.33 headers mishandle termios speeds.
    let base = || {
        let mut b = cc::Build::new();
        if std::env::var_os("CC").is_none() {
            b.compiler("gcc");
        }
        b.include(&blink)
            .include(blink.join("third_party/libz"))
            .opt_level(2)
            .debug(true)
            .warnings(false)
            .flag("-fno-common")
            .flag("-pthread")
            .flag("-fno-omit-frame-pointer")
            .flag("-fno-optimize-sibling-calls")
            .flag("-fcf-protection=none")
            .flag("-U_FORTIFY_SOURCE")
            .define("_FILE_OFFSET_BITS", "64")
            .define("_DEFAULT_SOURCE", None)
            .define("_BSD_SOURCE", None)
            .define("_GNU_SOURCE", None);
        b
    };
    let sha = git(&blink, &["rev-parse", "--verify", "HEAD"]);
    let commits = git(&blink, &["rev-list", "HEAD", "--count"]);
    let quoted = |s: &str| format!("\"{}\"", s);

    let mut main = base();
    let mut uop = base();
    let mut info = base();
    for (name, path) in &sources {
        match name.as_str() {
            "uop.c" => {
                uop.file(path);
            }
            "blink.c" | "syscall.c" => {
                info.file(path);
            }
            _ => {
                main.file(path);
            }
        }
    }
    // micro-ops must be compiled exactly as blink's Makefile does (UOPFLAGS)
    uop.flag("-fpatchable-function-entry=0,0")
        .flag("-fno-stack-protector")
        .flag("-fno-sanitize=all")
        .flag("-fomit-frame-pointer");
    // build info, plus renaming blink's C main() so Rust can call it
    info.define("main", "blink_c_main")
        .define("BUILD_MODE", quoted("blink-xbox").as_str())
        .define("BUILD_TOOLCHAIN", quoted("gcc via cc-rs").as_str())
        .define("BLINK_VERSION", quoted("1.1.0").as_str())
        .define("BLINK_COMMITS", quoted(&commits).as_str())
        .define("BLINK_GITSHA", quoted(&sha).as_str())
        .define("CONFIG_ARGUMENTS", quoted("").as_str())
        .define("BLINK_UNAME_V", quoted("DEFAULT").as_str());
    info.compile("blinkinfo");
    uop.compile("blinkuop");
    main.compile("blink");

    let mut z = base();
    for entry in std::fs::read_dir(blink.join("third_party/libz")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "c") {
            z.file(path);
        }
    }
    z.compile("blinkz");

    // blinkinfo references blink and blinkuop; list dependents first
    println!("cargo:rustc-link-lib=static=blinkinfo");
    println!("cargo:rustc-link-lib=static=blink");
    println!("cargo:rustc-link-lib=static=blinkuop");
    println!("cargo:rustc-link-lib=static=blinkz");
    for lib in ["pthread", "m", "rt"] {
        println!("cargo:rustc-link-lib={lib}");
    }
}
