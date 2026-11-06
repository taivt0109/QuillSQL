//! Console output shared by `quill schema` and `quill data`, laid out as SQL Compare's command
//! line does: a title block, normal lines on standard output, `Warning:` and `Error:` lines on
//! standard error, `/quiet` and `/verbose`, the `/outputwidth` used for tables, and the
//! `/logLevel` log file.

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Quiet,
    Normal,
    Verbose,
}

static MODE: AtomicU8 = AtomicU8::new(1);
static WIDTH: AtomicUsize = AtomicUsize::new(0);

/// `/logLevel`: what goes to the log file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    None,
    Error,
    Warning,
    Verbose,
}

static LOG: Mutex<Option<(std::fs::File, LogLevel)>> = Mutex::new(None);

pub fn set_mode(mode: Mode) {
    MODE.store(mode as u8, Ordering::Relaxed);
}

pub fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        0 => Mode::Quiet,
        2 => Mode::Verbose,
        _ => Mode::Normal,
    }
}

pub fn set_width(columns: usize) {
    WIDTH.store(columns, Ordering::Relaxed);
}

/// Columns used for layout: `/outputwidth`, else the console's buffer width, else 80.
pub fn width() -> usize {
    match WIDTH.load(Ordering::Relaxed) {
        0 => console_columns().filter(|c| *c >= 40).unwrap_or(80),
        w => w,
    }
}

#[cfg(windows)]
fn console_columns() -> Option<usize> {
    #[repr(C)]
    struct Coord {
        x: i16,
        y: i16,
    }
    #[repr(C)]
    struct Info {
        size: Coord,
        cursor: Coord,
        attributes: u16,
        window: [i16; 4],
        max: Coord,
    }
    unsafe extern "system" {
        fn GetStdHandle(n: u32) -> isize;
        fn GetConsoleScreenBufferInfo(h: isize, info: *mut Info) -> i32;
    }
    // Standard output, then standard error and input, as .NET does when output is redirected.
    for n in [-11i32 as u32, -12i32 as u32, -10i32 as u32] {
        let mut info = Info { size: Coord { x: 0, y: 0 }, cursor: Coord { x: 0, y: 0 }, attributes: 0, window: [0; 4], max: Coord { x: 0, y: 0 } };
        // SAFETY: plain Win32 calls with a correctly sized out-parameter.
        if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(n), &mut info) } != 0 && info.size.x > 0 {
            return Some(info.size.x as usize);
        }
    }
    None
}

#[cfg(not(windows))]
fn console_columns() -> Option<usize> {
    std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok())
}

/// Ctrl+C and Ctrl+Break stop the run with exit code 130.
pub fn exit_on_ctrl_c() {
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
        }
        unsafe extern "system" fn handler(kind: u32) -> i32 {
            // CTRL_C_EVENT, CTRL_BREAK_EVENT
            if kind <= 1 {
                let _ = std::io::stdout().flush();
                eprintln!("Execution stopped because Ctrl+Break.");
                std::process::exit(crate::args::exit::CANCELLED as i32);
            }
            0
        }
        // SAFETY: registers a handler with the documented signature.
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
}

/// Starts the `/logLevel` log: `QUILL_COMPARE_LOGPATH` (a file or folder), else
/// `%LOCALAPPDATA%\QuillSQL\Logs`.
pub fn start_log(level: LogLevel, product: &str) {
    if level == LogLevel::None {
        return;
    }
    let path = match std::env::var_os("QUILL_COMPARE_LOGPATH").map(std::path::PathBuf::from) {
        Some(p) if p.is_dir() => p.join("QuillCompare-CLI.log"),
        Some(p) => p,
        None => std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("QuillSQL")
            .join("Logs")
            .join("QuillCompare-CLI.log"),
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        *LOG.lock().unwrap() = Some((f, level));
        write_log(LogLevel::Verbose, &format!("{product} {} started", env!("CARGO_PKG_VERSION")));
    }
}

fn write_log(level: LogLevel, message: &str) {
    if let Some((f, max)) = LOG.lock().unwrap().as_mut()
        && level <= *max
    {
        let t = now();
        let now = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", t.year, t.month, t.day, t.hour, t.minute, t.second, t.millis);
        let tag = match level {
            LogLevel::Error => "ERROR",
            LogLevel::Warning => "WARN",
            _ => "INFO",
        };
        let _ = writeln!(f, "{now} {tag} {message}");
    }
}

/// A local date and time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millis: u16,
}

impl Stamp {
    /// `7 Nov 2026 2:41 PM`, the timestamp format of SQL Compare's reports.
    pub fn report_format(&self) -> String {
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let h12 = match self.hour % 12 {
            0 => 12,
            h => h,
        };
        let ampm = if self.hour < 12 { "AM" } else { "PM" };
        format!("{} {} {} {}:{:02} {ampm}", self.day, MONTHS[(self.month as usize).clamp(1, 12) - 1], self.year, h12, self.minute)
    }

    /// `MM/dd/yyyy HH:mm:ss`, as .NET writes a date with the invariant culture.
    pub fn invariant_format(&self) -> String {
        format!("{:02}/{:02}/{:04} {:02}:{:02}:{:02}", self.month, self.day, self.year, self.hour, self.minute, self.second)
    }

    /// ISO 8601 without zone.
    pub fn iso_format(&self) -> String {
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", self.year, self.month, self.day, self.hour, self.minute, self.second)
    }
}

/// The local time.
#[cfg(windows)]
pub fn now() -> Stamp {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    unsafe extern "system" {
        fn GetLocalTime(t: *mut SystemTime);
    }
    let mut t = SystemTime::default();
    // SAFETY: fills a correctly laid out SYSTEMTIME.
    unsafe { GetLocalTime(&mut t) };
    Stamp { year: t.year, month: t.month, day: t.day, hour: t.hour, minute: t.minute, second: t.second, millis: t.millis }
}

/// The time in UTC (no time zone database here).
#[cfg(not(windows))]
pub fn now() -> Stamp {
    utc(std::time::SystemTime::now())
}

/// A time as UTC.
pub fn utc(t: std::time::SystemTime) -> Stamp {
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Stamp {
        year: year as u16,
        month: month as u16,
        day: day as u16,
        hour: (rest / 3600) as u16,
        minute: (rest % 3600 / 60) as u16,
        second: (rest % 60) as u16,
        millis: d.subsec_millis() as u16,
    }
}

/// A normal line (hidden by `/quiet`).
pub fn log(message: &str) {
    write_log(LogLevel::Verbose, message);
    if mode() >= Mode::Normal {
        println!("{message}");
    }
}

/// A `/verbose` line.
pub fn verbose(message: &str) {
    write_log(LogLevel::Verbose, message);
    if mode() == Mode::Verbose {
        println!("{message}");
    }
}

pub fn is_verbose() -> bool {
    mode() == Mode::Verbose
}

/// `Warning: ...` on standard error (hidden by `/quiet`).
pub fn warn(message: &str) {
    write_log(LogLevel::Warning, message);
    if mode() >= Mode::Normal {
        let _ = std::io::stdout().flush();
        eprintln!("{}", fixed_width(&format!("Warning: {message}"), width(), "", true));
    }
}

/// `Error: ...` on standard error, also in quiet mode.
pub fn error(message: &str) {
    write_log(LogLevel::Error, message);
    let _ = std::io::stdout().flush();
    eprintln!("{}", fixed_width(&format!("Error: {message}"), width(), "", true));
}

/// The title block printed before anything else.
pub fn title(product: &str) {
    let w = width();
    println!("{}", fixed_width(&format!("{product} V{}", env!("CARGO_PKG_VERSION")), w, "", true));
    println!("{}", "=".repeat(w.saturating_sub(2)));
    println!("{}", fixed_width("Copyright (c) QuillSQL contributors. MIT License.", w, "", true));
    println!();
}

/// Wraps text at `width` columns without breaking words, each line starting with `prefix`.
/// `collapse` turns runs of white space into one space first.
pub fn fixed_width(source: &str, width: usize, prefix: &str, collapse: bool) -> String {
    let text = if collapse { source.split_whitespace().collect::<Vec<_>>().join(" ") } else { source.to_string() };
    let room = width.saturating_sub(prefix.chars().count()).max(10);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            out.push(prefix.to_string());
            continue;
        }
        let mut start = 0;
        while start < chars.len() {
            let mut end = (start + room).min(chars.len());
            if end < chars.len() {
                // Back up to a space so the word stays whole.
                if let Some(space) = (start + 1..=end).rev().find(|&i| chars[i].is_whitespace()) {
                    end = space;
                }
            }
            let piece: String = chars[start..end].iter().collect();
            // Each piece is trimmed, as SQL Compare does (so wrapped script lines lose their indent).
            out.push(format!("{prefix}{}", piece.trim()));
            start = end;
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_timestamps_like_net() {
        let t = Stamp { year: 2026, month: 11, day: 7, hour: 14, minute: 3, second: 9, millis: 0 };
        assert_eq!(t.report_format(), "7 Nov 2026 2:03 PM");
        assert_eq!(Stamp { hour: 0, ..t }.report_format(), "7 Nov 2026 12:03 AM");
        assert_eq!(t.invariant_format(), "11/07/2026 14:03:09");
        let then = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_794_060_189);
        assert_eq!(utc(then).iso_format(), "2026-11-07T14:03:09");
    }

    #[test]
    fn wraps_without_breaking_words() {
        assert_eq!(fixed_width("one two  three", 10, "", true), "one two\nthree");
        assert_eq!(fixed_width("CREATE TABLE x", 12, ">> ", false), ">> CREATE\n>> TABLE x");
        assert_eq!(fixed_width("short", 80, "== ", false), "== short");
    }
}
