//! A VT/xterm-compatible terminal: parses what programs write (UTF-8 text,
//! C0 controls, ESC/CSI/OSC sequences) into a grid of cells. What it
//! implements is what full-screen programs like Claude Code's TUI use:
//! cursor movement, erasing, scroll regions, insert/delete, SGR colors
//! (16, 256 and truecolor), the alternate screen, DEC private modes, and
//! replies to queries (device attributes, cursor position, mode reports,
//! colors). Replies collect in `responses` for the host to feed back as
//! keyboard input.

use std::collections::VecDeque;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

pub const BOLD: u8 = 1;
pub const DIM: u8 = 2;
pub const ITALIC: u8 = 4;
pub const UNDERLINE: u8 = 8;
pub const INVERSE: u8 = 16;
pub const HIDDEN: u8 = 32;
pub const STRIKE: u8 = 64;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attr: u8,
    /// 0 normal, 1 first half of a wide character, 2 its second half
    pub wide: u8,
}

impl Default for Cell {
    fn default() -> Cell {
        Cell { ch: ' ', fg: Color::Default, bg: Color::Default, attr: 0, wide: 0 }
    }
}

type Line = Vec<Cell>;

#[derive(Clone, Copy)]
struct Saved {
    x: usize,
    y: usize,
    pen: Cell,
    dec_graphics: bool,
    origin: bool,
}

enum State {
    Ground,
    Escape,
    /// ESC followed by an intermediate byte (charset designation etc.)
    EscInter(u8),
    Csi,
    Osc,
    /// DCS/SOS/PM/APC: skipped until the string terminator
    Str,
    /// ESC seen inside an OSC or other string
    StrEsc { osc: bool },
}

pub struct Term {
    pub rows: usize,
    pub cols: usize,
    lines: Vec<Line>,
    other: Vec<Line>,
    pub alt: bool,
    pub scrollback: VecDeque<Line>,
    pub cx: usize,
    pub cy: usize,
    wrap_pending: bool,
    pen: Cell,
    top: usize,
    bot: usize,
    saved: Saved,
    saved_alt: Saved,
    tabs: Vec<bool>,
    dec_graphics: bool,
    last_char: char,
    pub app_cursor: bool,
    pub app_keypad: bool,
    autowrap: bool,
    origin: bool,
    insert: bool,
    pub cursor_visible: bool,
    pub bracketed_paste: bool,
    pub title: String,
    state: State,
    params: Vec<Vec<u32>>,
    private: u8,
    inter: Vec<u8>,
    osc: Vec<u8>,
    utf8: Vec<u8>,
    /// replies for the program (append to its input)
    pub responses: Vec<u8>,
    /// something changed since the last draw
    pub dirty: bool,
}

const SCROLLBACK: usize = 5000;

fn blank_line(cols: usize, bg: Color) -> Line {
    vec![Cell { bg, ..Cell::default() }; cols]
}

impl Term {
    pub fn new(rows: usize, cols: usize) -> Term {
        let saved = Saved { x: 0, y: 0, pen: Cell::default(), dec_graphics: false, origin: false };
        Term {
            rows,
            cols,
            lines: vec![blank_line(cols, Color::Default); rows],
            other: vec![blank_line(cols, Color::Default); rows],
            alt: false,
            scrollback: VecDeque::new(),
            cx: 0,
            cy: 0,
            wrap_pending: false,
            pen: Cell::default(),
            top: 0,
            bot: rows - 1,
            saved,
            saved_alt: saved,
            tabs: (0..cols).map(|i| i % 8 == 0).collect(),
            dec_graphics: false,
            last_char: ' ',
            app_cursor: false,
            app_keypad: false,
            autowrap: true,
            origin: false,
            insert: false,
            cursor_visible: true,
            bracketed_paste: false,
            title: String::new(),
            state: State::Ground,
            params: Vec::new(),
            private: 0,
            inter: Vec::new(),
            osc: Vec::new(),
            utf8: Vec::new(),
            responses: Vec::new(),
            dirty: true,
        }
    }

    /// The visible screen, `offset` lines scrolled back into history.
    pub fn line(&self, y: usize, offset: usize) -> &Line {
        if offset == 0 || self.alt {
            return &self.lines[y];
        }
        let sb = self.scrollback.len();
        let back = offset.min(sb);
        if y < back {
            &self.scrollback[sb - back + y]
        } else {
            &self.lines[y - back]
        }
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        if rows == self.rows && cols == self.cols {
            return;
        }
        for lines in [&mut self.lines, &mut self.other] {
            for l in lines.iter_mut() {
                l.resize(cols, Cell::default());
            }
            while lines.len() > rows {
                let l = lines.remove(0);
                if !self.alt {
                    self.scrollback.push_back(l);
                }
                self.cy = self.cy.saturating_sub(1);
            }
            while lines.len() < rows {
                lines.push(blank_line(cols, Color::Default));
            }
        }
        self.rows = rows;
        self.cols = cols;
        self.top = 0;
        self.bot = rows - 1;
        self.cx = self.cx.min(cols - 1);
        self.cy = self.cy.min(rows - 1);
        self.tabs = (0..cols).map(|i| i % 8 == 0).collect();
        self.dirty = true;
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
        self.dirty = true;
    }

    fn byte(&mut self, b: u8) {
        // C0 controls act anywhere except inside strings
        if b < 0x20 && !matches!(self.state, State::Osc | State::Str | State::StrEsc { .. }) {
            match b {
                0x1b => {
                    self.state = State::Escape;
                    self.inter.clear();
                }
                0x18 | 0x1a => self.state = State::Ground,
                _ => self.control(b),
            }
            return;
        }
        match self.state {
            State::Ground => self.ground(b),
            State::Escape => self.escape(b),
            State::EscInter(i) => {
                self.esc_inter(i, b);
                self.state = State::Ground;
            }
            State::Csi => self.csi_byte(b),
            State::Osc => match b {
                0x07 => {
                    self.osc_done();
                    self.state = State::Ground;
                }
                0x1b => self.state = State::StrEsc { osc: true },
                _ => self.osc.push(b),
            },
            State::Str => {
                if b == 0x1b {
                    self.state = State::StrEsc { osc: false };
                } else if b == 0x07 {
                    self.state = State::Ground;
                }
            }
            State::StrEsc { osc } => {
                if b == b'\\' {
                    if osc {
                        self.osc_done();
                    }
                    self.state = State::Ground;
                } else if osc {
                    self.osc.push(0x1b);
                    self.osc.push(b);
                    self.state = State::Osc;
                } else {
                    self.state = State::Str;
                }
            }
        }
    }

    fn ground(&mut self, b: u8) {
        if b < 0x80 {
            self.utf8.clear();
            if b != 0x7f {
                self.print(b as char);
            }
            return;
        }
        if b & 0xc0 == 0x80 && !self.utf8.is_empty() {
            self.utf8.push(b);
        } else {
            self.utf8.clear();
            self.utf8.push(b);
        }
        let need = match self.utf8[0] {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => {
                self.utf8.clear();
                self.print('\u{fffd}');
                return;
            }
        };
        if self.utf8.len() == need {
            let c = std::str::from_utf8(&self.utf8).ok().and_then(|s| s.chars().next()).unwrap_or('\u{fffd}');
            self.utf8.clear();
            self.print(c);
        }
    }

    fn control(&mut self, b: u8) {
        match b {
            0x07 => {}
            0x08 => {
                self.wrap_pending = false;
                self.cx = self.cx.saturating_sub(1);
            }
            0x09 => self.tab(1),
            0x0a..=0x0c => self.linefeed(),
            0x0d => {
                self.wrap_pending = false;
                self.cx = 0;
            }
            _ => {}
        }
    }

    fn line_mut(&mut self, y: usize) -> &mut Line {
        &mut self.lines[y]
    }

    fn erase_cell(&self) -> Cell {
        Cell { bg: self.pen.bg, ..Cell::default() }
    }

    fn print(&mut self, c: char) {
        let c = if self.dec_graphics { dec_graphics(c) } else { c };
        let w = match c.width() {
            Some(0) => return, // combining marks: dropped
            Some(w) => w.min(2),
            None => return,
        };
        if self.wrap_pending && self.autowrap {
            self.cx = 0;
            self.linefeed();
        }
        self.wrap_pending = false;
        if w == 2 && self.cx == self.cols - 1 {
            if self.autowrap {
                let e = self.erase_cell();
                let (x, y) = (self.cx, self.cy);
                self.line_mut(y)[x] = e;
                self.cx = 0;
                self.linefeed();
            } else {
                return;
            }
        }
        let (x, y, cols) = (self.cx, self.cy, self.cols);
        if self.insert {
            let l = self.line_mut(y);
            for _ in 0..w {
                l.insert(x, Cell::default());
                l.truncate(cols);
            }
        }
        let pen = self.pen;
        let l = self.line_mut(y);
        // overwriting half of a wide character clears the other half
        if l[x].wide == 2 && x > 0 {
            l[x - 1] = Cell { ch: ' ', wide: 0, ..l[x - 1] };
        }
        if l[x].wide == 1 && x + 1 < cols {
            l[x + 1] = Cell { ch: ' ', wide: 0, ..l[x + 1] };
        }
        l[x] = Cell { ch: c, wide: if w == 2 { 1 } else { 0 }, ..pen };
        if w == 2 {
            l[x + 1] = Cell { ch: ' ', wide: 2, ..pen };
        }
        self.last_char = c;
        if self.cx + w >= self.cols {
            self.cx = self.cols - 1;
            self.wrap_pending = true;
        } else {
            self.cx += w;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        let e = self.erase_cell();
        for _ in 0..n.min(self.bot - self.top + 1) {
            let l = self.lines.remove(self.top);
            if self.top == 0 && !self.alt {
                self.scrollback.push_back(l);
                if self.scrollback.len() > SCROLLBACK {
                    self.scrollback.pop_front();
                }
            }
            self.lines.insert(self.bot, vec![e; self.cols]);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let e = self.erase_cell();
        for _ in 0..n.min(self.bot - self.top + 1) {
            self.lines.remove(self.bot);
            self.lines.insert(self.top, vec![e; self.cols]);
        }
    }

    fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.cy == self.bot {
            self.scroll_up(1);
        } else if self.cy + 1 < self.rows {
            self.cy += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.wrap_pending = false;
        if self.cy == self.top {
            self.scroll_down(1);
        } else {
            self.cy = self.cy.saturating_sub(1);
        }
    }

    fn tab(&mut self, n: usize) {
        for _ in 0..n {
            let mut x = self.cx + 1;
            while x < self.cols - 1 && !self.tabs[x] {
                x += 1;
            }
            self.cx = x.min(self.cols - 1);
        }
    }

    fn save(&mut self) -> Saved {
        Saved { x: self.cx, y: self.cy, pen: self.pen, dec_graphics: self.dec_graphics, origin: self.origin }
    }

    fn restore(&mut self, s: Saved) {
        self.cx = s.x.min(self.cols - 1);
        self.cy = s.y.min(self.rows - 1);
        self.pen = s.pen;
        self.dec_graphics = s.dec_graphics;
        self.origin = s.origin;
        self.wrap_pending = false;
    }

    fn set_alt(&mut self, on: bool, clear: bool) {
        if on == self.alt {
            return;
        }
        std::mem::swap(&mut self.lines, &mut self.other);
        self.alt = on;
        if on && clear {
            let e = Cell::default();
            for l in self.lines.iter_mut() {
                l.fill(e);
            }
        }
    }

    fn reset(&mut self) {
        let (r, c) = (self.rows, self.cols);
        let sb = std::mem::take(&mut self.scrollback);
        *self = Term::new(r, c);
        self.scrollback = sb;
    }

    fn escape(&mut self, b: u8) {
        self.state = State::Ground;
        match b {
            b'[' => {
                self.state = State::Csi;
                self.params.clear();
                self.params.push(Vec::new());
                self.private = 0;
                self.inter.clear();
            }
            b']' => {
                self.state = State::Osc;
                self.osc.clear();
            }
            b'P' | b'X' | b'^' | b'_' => self.state = State::Str,
            b' '..=b'/' => self.state = State::EscInter(b),
            b'7' => self.saved = self.save(),
            b'8' => {
                let s = self.saved;
                self.restore(s);
            }
            b'D' => self.linefeed(),
            b'E' => {
                self.cx = 0;
                self.linefeed();
            }
            b'H' => {
                let x = self.cx;
                self.tabs[x] = true;
            }
            b'M' => self.reverse_index(),
            b'c' => self.reset(),
            b'=' => self.app_keypad = true,
            b'>' => self.app_keypad = false,
            _ => {}
        }
    }

    fn esc_inter(&mut self, i: u8, b: u8) {
        match (i, b) {
            (b'(', b'0') => self.dec_graphics = true,
            (b'(', _) => self.dec_graphics = false,
            (b'#', b'8') => {
                // DECALN: fill with E
                for l in self.lines.iter_mut() {
                    l.fill(Cell { ch: 'E', ..Cell::default() });
                }
            }
            _ => {}
        }
    }

    fn csi_byte(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                let g = self.params.last_mut().unwrap();
                if g.is_empty() {
                    g.push(0);
                }
                let v = g.last_mut().unwrap();
                *v = v.saturating_mul(10).saturating_add((b - b'0') as u32);
            }
            b';' => self.params.push(Vec::new()),
            b':' => {
                let g = self.params.last_mut().unwrap();
                if g.is_empty() {
                    g.push(0);
                }
                g.push(0);
            }
            b'<'..=b'?' => self.private = b,
            b' '..=b'/' => self.inter.push(b),
            0x40..=0x7e => {
                self.state = State::Ground;
                self.csi(b);
            }
            _ => self.state = State::Ground,
        }
    }

    /// Parameter `i` (first sub-parameter), or `d` when missing or zero.
    fn p(&self, i: usize, d: usize) -> usize {
        match self.params.get(i).and_then(|g| g.first()) {
            Some(&v) if v > 0 => v as usize,
            _ => d,
        }
    }

    fn p0(&self, i: usize) -> usize {
        self.params.get(i).and_then(|g| g.first()).copied().unwrap_or(0) as usize
    }

    fn reply(&mut self, s: &str) {
        self.responses.extend_from_slice(s.as_bytes());
    }

    fn csi(&mut self, f: u8) {
        let (rows, cols) = (self.rows, self.cols);
        self.wrap_pending = false;
        match (self.private, self.inter.first().copied(), f) {
            (0, None, b'@') => {
                let (x, y, n, e) = (self.cx, self.cy, self.p(0, 1), self.erase_cell());
                let l = self.line_mut(y);
                for _ in 0..n.min(cols - x) {
                    l.insert(x, e);
                    l.truncate(cols);
                }
            }
            (0, None, b'A') => self.cy = self.cy.saturating_sub(self.p(0, 1)).max(if self.cy >= self.top { self.top } else { 0 }),
            (0, None, b'B') | (0, None, b'e') => self.cy = (self.cy + self.p(0, 1)).min(if self.cy <= self.bot { self.bot } else { rows - 1 }),
            (0, None, b'C') | (0, None, b'a') => self.cx = (self.cx + self.p(0, 1)).min(cols - 1),
            (0, None, b'D') => self.cx = self.cx.saturating_sub(self.p(0, 1)),
            (0, None, b'E') => {
                self.cy = (self.cy + self.p(0, 1)).min(rows - 1);
                self.cx = 0;
            }
            (0, None, b'F') => {
                self.cy = self.cy.saturating_sub(self.p(0, 1));
                self.cx = 0;
            }
            (0, None, b'G') | (0, None, b'`') => self.cx = (self.p(0, 1) - 1).min(cols - 1),
            (0, None, b'H') | (0, None, b'f') => {
                let base = if self.origin { self.top } else { 0 };
                self.cy = (base + self.p(0, 1) - 1).min(rows - 1);
                self.cx = (self.p(1, 1) - 1).min(cols - 1);
            }
            (0, None, b'I') => self.tab(self.p(0, 1)),
            (0, None, b'Z') => {
                for _ in 0..self.p(0, 1) {
                    let mut x = self.cx.saturating_sub(1);
                    while x > 0 && !self.tabs[x] {
                        x -= 1;
                    }
                    self.cx = x;
                }
            }
            (_, None, b'J') => {
                let e = self.erase_cell();
                let (x, y) = (self.cx, self.cy);
                match self.p0(0) {
                    0 => {
                        self.line_mut(y)[x..].fill(e);
                        for l in &mut self.lines[y + 1..] {
                            l.fill(e);
                        }
                    }
                    1 => {
                        self.line_mut(y)[..=x].fill(e);
                        for l in &mut self.lines[..y] {
                            l.fill(e);
                        }
                    }
                    2 => {
                        for l in self.lines.iter_mut() {
                            l.fill(e);
                        }
                    }
                    3 => self.scrollback.clear(),
                    _ => {}
                }
            }
            (_, None, b'K') => {
                let e = self.erase_cell();
                let (x, y, mode) = (self.cx, self.cy, self.p0(0));
                let l = self.line_mut(y);
                match mode {
                    0 => l[x..].fill(e),
                    1 => l[..=x].fill(e),
                    2 => l.fill(e),
                    _ => {}
                }
            }
            (0, None, b'L') => {
                if (self.top..=self.bot).contains(&self.cy) {
                    let e = self.erase_cell();
                    for _ in 0..self.p(0, 1).min(self.bot - self.cy + 1) {
                        self.lines.remove(self.bot);
                        self.lines.insert(self.cy, vec![e; cols]);
                    }
                    self.cx = 0;
                }
            }
            (0, None, b'M') => {
                if (self.top..=self.bot).contains(&self.cy) {
                    let e = self.erase_cell();
                    for _ in 0..self.p(0, 1).min(self.bot - self.cy + 1) {
                        self.lines.remove(self.cy);
                        self.lines.insert(self.bot, vec![e; cols]);
                    }
                    self.cx = 0;
                }
            }
            (0, None, b'P') => {
                let (x, y, n, e) = (self.cx, self.cy, self.p(0, 1), self.erase_cell());
                let l = self.line_mut(y);
                for _ in 0..n.min(cols - x) {
                    l.remove(x);
                    l.push(e);
                }
            }
            (0, None, b'S') => self.scroll_up(self.p(0, 1)),
            (0, None, b'T') => self.scroll_down(self.p(0, 1)),
            (0, None, b'X') => {
                let (x, y, n, e) = (self.cx, self.cy, self.p(0, 1), self.erase_cell());
                let end = (x + n).min(cols);
                self.line_mut(y)[x..end].fill(e);
            }
            (0, None, b'b') => {
                let c = self.last_char;
                for _ in 0..self.p(0, 1).min(cols * rows) {
                    self.print(c);
                }
            }
            (0, None, b'c') => self.reply("\x1b[?62;22c"),
            (b'>', None, b'c') => self.reply("\x1b[>1;10;0c"),
            (0, None, b'd') => {
                let base = if self.origin { self.top } else { 0 };
                self.cy = (base + self.p(0, 1) - 1).min(rows - 1);
            }
            (0, None, b'g') => match self.p0(0) {
                0 => {
                    let x = self.cx;
                    self.tabs[x] = false;
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            (0, None, b'h') | (0, None, b'l') => {
                let on = f == b'h';
                for i in 0..self.params.len() {
                    if self.p0(i) == 4 {
                        self.insert = on;
                    }
                }
            }
            (b'?', None, b'h') | (b'?', None, b'l') => {
                let on = f == b'h';
                for i in 0..self.params.len() {
                    self.dec_mode(self.p0(i), on);
                }
            }
            (0, None, b'm') => self.sgr(),
            (0, None, b'n') => match self.p0(0) {
                5 => self.reply("\x1b[0n"),
                6 => {
                    let s = format!("\x1b[{};{}R", self.cy + 1, self.cx + 1);
                    self.reply(&s);
                }
                _ => {}
            },
            (b'?', None, b'n') if self.p0(0) == 6 => {
                let s = format!("\x1b[?{};{}R", self.cy + 1, self.cx + 1);
                self.reply(&s);
            }
            // DECRQM: report private modes we know (1 set, 2 reset, 0 unknown)
            (b'?', Some(b'$'), b'p') => {
                let m = self.p0(0);
                let v = match m {
                    1 => self.app_cursor as u8 + (!self.app_cursor) as u8 * 2,
                    7 => if self.autowrap { 1 } else { 2 },
                    25 => if self.cursor_visible { 1 } else { 2 },
                    1049 | 1047 | 47 => if self.alt { 1 } else { 2 },
                    2004 => if self.bracketed_paste { 1 } else { 2 },
                    2026 => 2, // synchronized output: supported, never "on" here
                    _ => 0,
                };
                let s = format!("\x1b[?{m};{v}$y");
                self.reply(&s);
            }
            (0, None, b'r') => {
                let t = self.p(0, 1) - 1;
                let b = self.p(1, rows) - 1;
                if t < b && b < rows {
                    self.top = t;
                    self.bot = b;
                    self.cx = 0;
                    self.cy = if self.origin { t } else { 0 };
                }
            }
            (0, None, b's') => self.saved = self.save(),
            (0, None, b'u') => {
                let s = self.saved;
                self.restore(s);
            }
            (0, None, b't') => {
                if self.p0(0) == 18 {
                    let s = format!("\x1b[8;{rows};{cols}t");
                    self.reply(&s);
                }
            }
            _ => {} // cursor style, kitty keyboard queries (unsupported: no reply), etc.
        }
    }

    fn dec_mode(&mut self, m: usize, on: bool) {
        match m {
            1 => self.app_cursor = on,
            6 => {
                self.origin = on;
                self.cx = 0;
                self.cy = if on { self.top } else { 0 };
            }
            7 => self.autowrap = on,
            25 => self.cursor_visible = on,
            47 | 1047 => self.set_alt(on, m == 1047),
            1048 => {
                if on {
                    self.saved = self.save();
                } else {
                    let s = self.saved;
                    self.restore(s);
                }
            }
            1049 => {
                if on {
                    self.saved_alt = self.save();
                    self.set_alt(true, true);
                } else {
                    self.set_alt(false, false);
                    let s = self.saved_alt;
                    self.restore(s);
                }
            }
            2004 => self.bracketed_paste = on,
            _ => {} // mouse reporting, focus events, sync output: accepted, ignored
        }
    }

    fn sgr(&mut self) {
        if self.params.iter().all(|g| g.is_empty()) {
            self.pen = Cell { ch: ' ', wide: 0, ..Cell::default() };
            return;
        }
        let groups = self.params.clone();
        let mut i = 0;
        while i < groups.len() {
            let g = &groups[i];
            let v = g.first().copied().unwrap_or(0);
            match v {
                0 => {
                    self.pen.fg = Color::Default;
                    self.pen.bg = Color::Default;
                    self.pen.attr = 0;
                }
                1 => self.pen.attr |= BOLD,
                2 => self.pen.attr |= DIM,
                3 => self.pen.attr |= ITALIC,
                4 => {
                    if g.get(1) == Some(&0) {
                        self.pen.attr &= !UNDERLINE;
                    } else {
                        self.pen.attr |= UNDERLINE;
                    }
                }
                7 => self.pen.attr |= INVERSE,
                8 => self.pen.attr |= HIDDEN,
                9 => self.pen.attr |= STRIKE,
                21 => self.pen.attr |= UNDERLINE,
                22 => self.pen.attr &= !(BOLD | DIM),
                23 => self.pen.attr &= !ITALIC,
                24 => self.pen.attr &= !UNDERLINE,
                27 => self.pen.attr &= !INVERSE,
                28 => self.pen.attr &= !HIDDEN,
                29 => self.pen.attr &= !STRIKE,
                30..=37 => self.pen.fg = Color::Idx((v - 30) as u8),
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Idx((v - 40) as u8),
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = Color::Idx((v - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Idx((v - 100 + 8) as u8),
                38 | 48 | 58 => {
                    // colon form: all in this group; else in the next groups
                    let (color, used) = if g.len() > 1 {
                        (extended_color(&g[1..], true), 0)
                    } else {
                        let rest: Vec<u32> = groups[i + 1..].iter().map(|g| g.first().copied().unwrap_or(0)).collect();
                        let c = extended_color(&rest, false);
                        let used = match rest.first() {
                            Some(5) => 2,
                            Some(2) => 4,
                            _ => 0,
                        };
                        (c, used)
                    };
                    if let Some(c) = color {
                        match v {
                            38 => self.pen.fg = c,
                            48 => self.pen.bg = c,
                            _ => {}
                        }
                    }
                    i += used;
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn osc_done(&mut self) {
        let s = String::from_utf8_lossy(&self.osc).into_owned();
        let (num, data) = s.split_once(';').unwrap_or((&s, ""));
        match num {
            "0" | "2" => self.title = data.to_string(),
            // color queries: answer with our default colors
            "10" if data == "?" => self.reply("\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\"),
            "11" if data == "?" => self.reply("\x1b]11;rgb:1010/1010/1010\x1b\\"),
            "12" if data == "?" => self.reply("\x1b]12;rgb:d0d0/d0d0/d0d0\x1b\\"),
            _ => {}
        }
    }
}

/// 38/48 arguments: [5, n] or [2, r, g, b] (colon form may have a color
/// space id before r: [2, cs, r, g, b]).
fn extended_color(a: &[u32], colon: bool) -> Option<Color> {
    match a.first()? {
        5 => Some(Color::Idx(*a.get(1)? as u8)),
        2 => {
            let rgb = if colon && a.len() >= 5 { &a[2..5] } else { a.get(1..4)? };
            Some(Color::Rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))
        }
        _ => None,
    }
}

/// The DEC special graphics character set (ESC ( 0), used for line drawing.
fn dec_graphics(c: char) -> char {
    match c {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => c,
    }
}

/// xterm's 256-color palette, as sRGB.
pub fn palette(i: u8) -> [u8; 3] {
    const BASE: [[u8; 3]; 16] = [
        [0x1c, 0x1c, 0x1c],
        [0xe0, 0x5a, 0x5a],
        [0x6a, 0xc9, 0x6a],
        [0xd8, 0xc0, 0x5a],
        [0x5a, 0x8f, 0xe0],
        [0xc0, 0x78, 0xd8],
        [0x5a, 0xc8, 0xc8],
        [0xc8, 0xc8, 0xc8],
        [0x68, 0x68, 0x68],
        [0xff, 0x7a, 0x7a],
        [0x8a, 0xe8, 0x8a],
        [0xf0, 0xdc, 0x7a],
        [0x7a, 0xaf, 0xff],
        [0xe0, 0x98, 0xf8],
        [0x7a, 0xe8, 0xe8],
        [0xf2, 0xf2, 0xf2],
    ];
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let n = i - 16;
            let lvl = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            [lvl(n / 36), lvl(n / 6 % 6), lvl(n % 6)]
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            [g, g, g]
        }
    }
}

pub const DEFAULT_FG: [u8; 3] = [0xd0, 0xd0, 0xd0];
pub const DEFAULT_BG: [u8; 3] = [0x10, 0x10, 0x10];

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &Term, y: usize) -> String {
        t.line(y, 0).iter().filter(|c| c.wide != 2).map(|c| c.ch).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn prints_and_wraps() {
        let mut t = Term::new(3, 5);
        t.feed(b"hello world");
        assert_eq!(text(&t, 0), "hello");
        assert_eq!(text(&t, 1), " worl");
        assert_eq!(text(&t, 2), "d");
    }

    #[test]
    fn cursor_and_erase() {
        let mut t = Term::new(3, 10);
        t.feed(b"abcdef\x1b[1;3H\x1b[K");
        assert_eq!(text(&t, 0), "ab");
        t.feed(b"\x1b[2;1Hxyz\x1b[2J");
        assert_eq!(text(&t, 1), "");
    }

    #[test]
    fn scroll_region_and_scrollback() {
        let mut t = Term::new(3, 4);
        t.feed(b"1\r\n2\r\n3\r\n4");
        assert_eq!(text(&t, 0), "2");
        assert_eq!(t.scrollback.len(), 1);
        t.feed(b"\x1b[2;3r\x1b[3;1H\n");
        assert_eq!(text(&t, 0), "2");
    }

    #[test]
    fn sgr_colors() {
        let mut t = Term::new(1, 10);
        t.feed(b"\x1b[1;31ma\x1b[38;5;200mb\x1b[38;2;1;2;3mc\x1b[38:2::4:5:6md\x1b[0me");
        let l = t.line(0, 0);
        assert_eq!(l[0].fg, Color::Idx(1));
        assert_eq!(l[0].attr & BOLD, BOLD);
        assert_eq!(l[1].fg, Color::Idx(200));
        assert_eq!(l[2].fg, Color::Rgb(1, 2, 3));
        assert_eq!(l[3].fg, Color::Rgb(4, 5, 6));
        assert_eq!(l[4].fg, Color::Default);
    }

    #[test]
    fn wide_chars_and_utf8() {
        let mut t = Term::new(1, 6);
        t.feed("a日b⏺".as_bytes());
        let l = t.line(0, 0);
        assert_eq!(l[1].ch, '日');
        assert_eq!(l[1].wide, 1);
        assert_eq!(l[2].wide, 2);
        assert_eq!(l[3].ch, 'b');
    }

    #[test]
    fn alt_screen_and_replies() {
        let mut t = Term::new(2, 10);
        t.feed(b"main\x1b[?1049h\x1b[HALT\x1b[6n\x1b[c");
        assert_eq!(text(&t, 0), "ALT");
        assert_eq!(t.responses, b"\x1b[1;4R\x1b[?62;22c");
        t.feed(b"\x1b[?1049l");
        assert_eq!(text(&t, 0), "main");
    }

    #[test]
    fn osc_and_dcs_are_skipped() {
        let mut t = Term::new(1, 20);
        t.feed(b"\x1b]0;title\x07a\x1b]8;;http://x\x1b\\b\x1bPq#0\x1b\\c");
        assert_eq!(text(&t, 0), "abc");
        assert_eq!(t.title, "title");
    }
}
