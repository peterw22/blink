//! Unpacks a Linux root filesystem from a tar archive into a directory in
//! the form fs.rs reads: symlinks become marker files (UWP apps can't make
//! real ones), hard links become copies, device nodes and fifos are
//! skipped. Handles ustar, GNU long names and pax path/linkpath records.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

/// Extraction totals, for logging.
#[derive(Default, Debug)]
pub struct Stats {
    pub files: usize,
    pub dirs: usize,
    pub links: usize,
    pub skipped: usize,
}

fn octal(field: &[u8]) -> u64 {
    // base-256 for big files (GNU)
    if field[0] & 0x80 != 0 {
        return field[1..].iter().fold(0u64, |n, &b| (n << 8) | b as u64);
    }
    let s = String::from_utf8_lossy(field);
    u64::from_str_radix(s.trim_matches(|c: char| c == '\0' || c == ' '), 8).unwrap_or(0)
}

fn cstr(field: &[u8]) -> String {
    let n = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..n]).into_owned()
}

/// A path inside the archive, made relative and free of `..`.
fn safe(dest: &Path, name: &str) -> Option<PathBuf> {
    let mut p = dest.to_path_buf();
    let mut any = false;
    for c in Path::new(name.trim_start_matches('/')).components() {
        match c {
            Component::Normal(s) => {
                p.push(s);
                any = true;
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    any.then_some(p)
}

fn pax_records(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = data;
    while let Some(sp) = rest.iter().position(|&b| b == b' ') {
        let Ok(len) = String::from_utf8_lossy(&rest[..sp]).parse::<usize>() else { break };
        if len == 0 || len > rest.len() {
            break;
        }
        let rec = String::from_utf8_lossy(&rest[sp + 1..len - 1]).into_owned();
        if let Some((k, v)) = rec.split_once('=') {
            out.push((k.to_string(), v.to_string()));
        }
        rest = &rest[len..];
    }
    out
}

/// Writes a symlink marker (see fs.rs) and gives it the SYSTEM attribute.
pub fn write_link(path: &Path, target: &str) -> io::Result<()> {
    let _ = fs::remove_file(path);
    let mut data = super::fs::LINK_MAGIC.to_vec();
    data.extend_from_slice(target.as_bytes());
    fs::write(path, data)?;
    let w = super::fs::wide(&path.to_string_lossy());
    // FILE_ATTRIBUTE_SYSTEM
    unsafe { windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(w.as_ptr(), 4) };
    Ok(())
}

/// Unpacks `archive` into `dest`.
pub fn extract(archive: &Path, dest: &Path) -> io::Result<Stats> {
    let mut r = io::BufReader::with_capacity(1 << 20, fs::File::open(archive)?);
    fs::create_dir_all(dest)?;
    let mut st = Stats::default();
    let mut long_name: Option<String> = None;
    let mut long_link: Option<String> = None;
    let mut hdr = [0u8; 512];
    loop {
        if r.read_exact(&mut hdr).is_err() || hdr.iter().all(|&b| b == 0) {
            break;
        }
        let size = octal(&hdr[124..136]);
        let kind = hdr[156];
        let mut name = cstr(&hdr[0..100]);
        if &hdr[257..262] == b"ustar" {
            let prefix = cstr(&hdr[345..500]);
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        let mut link = cstr(&hdr[157..257]);
        let padded = size.div_ceil(512) * 512;
        // records that describe the next entry
        if matches!(kind, b'L' | b'K' | b'x' | b'g') {
            let mut data = vec![0u8; padded as usize];
            r.read_exact(&mut data)?;
            data.truncate(size as usize);
            match kind {
                b'L' => long_name = Some(cstr(&data)),
                b'K' => long_link = Some(cstr(&data)),
                b'x' => {
                    for (k, v) in pax_records(&data) {
                        match k.as_str() {
                            "path" => long_name = Some(v),
                            "linkpath" => long_link = Some(v),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if let Some(n) = long_name.take() {
            name = n;
        }
        if let Some(l) = long_link.take() {
            link = l;
        }
        let Some(path) = safe(dest, &name) else {
            io::copy(&mut (&mut r).take(padded), &mut io::sink())?;
            st.skipped += 1;
            continue;
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        match kind {
            b'5' => {
                fs::create_dir_all(&path)?;
                st.dirs += 1;
            }
            b'2' => {
                write_link(&path, &link)?;
                st.links += 1;
            }
            b'1' => {
                // hard link: copy what it names (already extracted)
                if let Some(src) = safe(dest, &link) {
                    let _ = fs::remove_file(&path);
                    fs::copy(&src, &path)?;
                    st.files += 1;
                }
            }
            b'0' | 0 | b'7' => {
                let mut f = fs::File::create(&path)?;
                io::copy(&mut (&mut r).take(size), &mut f)?;
                f.flush()?;
                io::copy(&mut (&mut r).take(padded - size), &mut io::sink())?;
                st.files += 1;
                continue;
            }
            _ => st.skipped += 1, // devices, fifos
        }
        io::copy(&mut (&mut r).take(padded), &mut io::sink())?;
    }
    Ok(st)
}
