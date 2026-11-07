//! Values as the text SQL Server's CONVERT styles give them, for sql_variant values.

use chrono::Datelike;

use super::values::{DAY, xtype};
use crate::data::value::Value;

/// `CONVERT(nvarchar, f, 3)`: 17 significant digits for float, 9 for real, `e+nnn` exponent.
pub fn float_style3(f: f64, real: bool) -> String {
    if f == 0.0 {
        return if real { "0.00000000e+000".into() } else { "0.0000000000000000e+000".into() };
    }
    let digits = if real { 8 } else { 16 };
    let s = format!("{:.*e}", digits, f);
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    format!("{mantissa}e{}{:03}", if exp < 0 { '-' } else { '+' }, exp.abs())
}

/// `CONVERT(nvarchar, m, 2)`: four decimals, no separators.
pub fn money_style2(v: i128) -> String {
    let sign = if v < 0 { "-" } else { "" };
    let a = v.unsigned_abs();
    format!("{sign}{}.{:04}", a / 10_000, a % 10_000)
}

fn ymd(days: i64) -> String {
    let d = chrono::NaiveDate::from_ymd_opt(1, 1, 1).unwrap() + chrono::Duration::days(days);
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

fn hms(ns: i64, digits: u8) -> String {
    let secs = ns / 1_000_000_000;
    let mut s = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    if digits > 0 {
        let frac = ns % 1_000_000_000 / 10i64.pow(9 - digits as u32);
        s.push_str(&format!(".{:0width$}", frac, width = digits as usize));
    }
    s
}

/// `CONVERT(nvarchar, d, 121)`.
pub fn odbc(v: &Value, base: u8, scale: u8) -> String {
    match *v {
        Value::Date(d) => ymd(d),
        Value::Time(t) => hms(t, scale),
        Value::DateTime(n) => {
            let (days, ns) = (n.div_euclid(DAY) as i64, n.rem_euclid(DAY) as i64);
            match base {
                xtype::DATETIME => {
                    // datetime shows milliseconds rounded from 1/300 s ticks.
                    let ticks = (ns % 1_000_000_000) * 300 / 1_000_000_000;
                    let ms = (ticks * 10 + 1) / 3;
                    format!("{} {}.{:03}", ymd(days), hms(ns - ns % 1_000_000_000, 0), ms)
                }
                xtype::SMALLDATETIME => format!("{} {}.000", ymd(days), hms(ns, 0)),
                _ => format!("{} {}", ymd(days), hms(ns, scale)),
            }
        }
        Value::DateTimeOffset(n, offset) => {
            let local = n + offset as i128 * 60_000_000_000;
            let (days, ns) = (local.div_euclid(DAY) as i64, local.rem_euclid(DAY) as i64);
            let sign = if offset < 0 { '-' } else { '+' };
            format!("{} {} {sign}{:02}:{:02}", ymd(days), hms(ns, scale), offset.abs() / 60, offset.abs() % 60)
        }
        _ => v.display(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_styles() {
        assert_eq!(float_style3(1.5, false), "1.5000000000000000e+000");
        assert_eq!(float_style3(-0.00125, true), "-1.25000000e-003");
        assert_eq!(money_style2(-123_4500), "-123.4500");
        assert_eq!(odbc(&Value::Date(0), xtype::DATE, 0), "0001-01-01");
        assert_eq!(odbc(&Value::Time(3_723_500_000_000), xtype::TIME, 3), "01:02:03.500");
    }
}
