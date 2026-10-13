//! Keyboard to terminal input bytes, the way xterm encodes keys.
//!
//! CoreWindow reports typed characters through CharacterReceived (including
//! control codes for Ctrl+letter) and every key through KeyDown. Keys that
//! type nothing (arrows, Home, F-keys...) and the few whose bytes differ
//! from Windows' (Backspace, Tab, Esc, Enter) are handled on KeyDown;
//! CharacterReceived then skips their codes.

/// Modifier state when a key went down.
#[derive(Clone, Copy, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Mods {
    /// xterm's modifier parameter: 1 + shift + 2*alt + 4*ctrl
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
}

/// Character codes CharacterReceived also delivers for keys `key_down`
/// handles; they must be dropped there.
pub fn handled_on_key_down(code: u32) -> bool {
    matches!(code, 8 | 9 | 13 | 27)
}

/// Bytes for a key press (Windows.System.VirtualKey), or None when the key
/// types a character and CharacterReceived will report it.
pub fn key_down(vk: i32, m: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    let csi_mod = |final_: char| -> Vec<u8> {
        if m.param() > 1 {
            format!("\x1b[1;{}{final_}", m.param()).into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_}").into_bytes()
        } else {
            format!("\x1b[{final_}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if m.param() > 1 {
            format!("\x1b[{n};{}~", m.param()).into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let alt = |b: &[u8]| -> Vec<u8> {
        let mut v = if m.alt { vec![0x1b] } else { Vec::new() };
        v.extend_from_slice(b);
        v
    };
    Some(match vk {
        8 => alt(if m.ctrl { b"\x08" } else { b"\x7f" }),
        9 => {
            if m.shift {
                b"\x1b[Z".to_vec()
            } else {
                alt(b"\t")
            }
        }
        13 => {
            if m.shift || m.alt {
                b"\x1b\r".to_vec() // newline in Claude Code's prompt
            } else {
                b"\r".to_vec()
            }
        }
        27 => b"\x1b".to_vec(),
        32 if m.ctrl => vec![0],
        33 => tilde(5),  // PageUp
        34 => tilde(6),  // PageDown
        35 => csi_mod('F'), // End
        36 => csi_mod('H'), // Home
        37 => csi_mod('D'),
        38 => csi_mod('A'),
        39 => csi_mod('C'),
        40 => csi_mod('B'),
        45 => tilde(2), // Insert
        46 => tilde(3), // Delete
        112..=115 => {
            // F1-F4
            let c = [b'P', b'Q', b'R', b'S'][(vk - 112) as usize] as char;
            if m.param() > 1 {
                format!("\x1b[1;{}{c}", m.param()).into_bytes()
            } else {
                format!("\x1bO{c}").into_bytes()
            }
        }
        116..=123 => tilde([15, 17, 18, 19, 20, 21, 23, 24][(vk - 116) as usize]), // F5-F12
        _ => return None,
    })
}

/// Bytes for a typed character (CharacterReceived), as UTF-8.
pub fn character(c: char, m: Mods) -> Vec<u8> {
    let mut v = if m.alt && !m.ctrl { vec![0x1b] } else { Vec::new() };
    let mut buf = [0u8; 4];
    v.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_and_modes() {
        let none = Mods::default();
        assert_eq!(key_down(38, none, false).unwrap(), b"\x1b[A");
        assert_eq!(key_down(38, none, true).unwrap(), b"\x1bOA");
        let ctrl = Mods { ctrl: true, ..none };
        assert_eq!(key_down(39, ctrl, true).unwrap(), b"\x1b[1;5C");
        let shift = Mods { shift: true, ..none };
        assert_eq!(key_down(9, shift, false).unwrap(), b"\x1b[Z");
        assert_eq!(key_down(8, none, false).unwrap(), b"\x7f");
        assert_eq!(key_down(65, none, false), None);
        assert_eq!(character('é', Mods { alt: true, ..none }), "\x1bé".as_bytes());
    }
}
