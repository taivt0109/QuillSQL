//! xml values as stored: a token stream (signature 0xDFFF, version, code page) that defines
//! names and qualified names before use, then elements, attributes, text, comments and
//! processing instructions. Written back as the text `CONVERT(nvarchar(max), x, 1)` gives.

use super::values::utf16;

const NAME: u8 = 0xF0;
const QNAME: u8 = 0xEF;
const ELEMENT: u8 = 0xF8;
const END_ELEMENT: u8 = 0xF7;
const ATTRIBUTE: u8 = 0xF6;
const END_ATTRIBUTES: u8 = 0xF5;
const PI: u8 = 0xF4;
const COMMENT: u8 = 0xF3;
const TEXT: u8 = 0x11;
const XML_DECLARATION: u8 = 0xFE;
/// An extension the reader can skip: length, then data (typed xml's type annotations).
const EXTENSION: u8 = 0xEA;

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let x = *self.b.get(self.at).ok_or("xml value ends early")?;
        self.at += 1;
        Ok(x)
    }

    /// A number in 7-bit groups, low group first, high bit = more.
    fn number(&mut self) -> Result<usize, String> {
        let mut v = 0usize;
        for shift in (0..35).step_by(7) {
            let x = self.byte()?;
            v |= ((x & 0x7f) as usize) << shift;
            if x & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("invalid length in xml value".into())
    }

    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let s = self.b.get(self.at..self.at + n).ok_or("xml value ends early")?;
        self.at += n;
        Ok(s)
    }

    /// A typed value (typed xml) as its canonical XSD text; `None` for tokens that are not values.
    fn atomic(&mut self, token: u8) -> Result<Option<String>, String> {
        let int = |b: &[u8]| b.iter().rev().fold(0i64, |v, &x| (v << 8) | x as i64);
        Ok(Some(match token {
            TEXT => self.text()?,
            0x01 => (int(self.take(2)?) as i16).to_string(),
            0x02 => (int(self.take(4)?) as i32).to_string(),
            0x07 => self.take(1)?[0].to_string(),
            0x08 => int(self.take(8)?).to_string(),
            0x06 | 0x86 => if self.take(1)?[0] != 0 { "true" } else { "false" }.to_string(),
            0x03 => double_text(f32::from_le_bytes(self.take(4)?.try_into().unwrap()) as f64),
            0x04 => double_text(f64::from_le_bytes(self.take(8)?.try_into().unwrap())),
            // Decimal: length, precision, scale, sign, magnitude.
            0x0A | 0x0B | 0x87 => {
                let n = self.take(1)?[0] as usize;
                let b = self.take(n)?;
                let scale = b[1] as u32;
                let mag = b[3..].iter().rev().fold(0u128, |v, &x| (v << 8) | x as u128);
                let p = 10u128.pow(scale);
                let mut t = format!("{}{}", if b[2] == 1 { "" } else { "-" }, mag / p);
                let frac = format!("{:0w$}", mag % p, w = scale as usize);
                let frac = frac.trim_end_matches('0');
                if !frac.is_empty() {
                    t.push('.');
                    t.push_str(frac);
                }
                if t == "-0" { "0".into() } else { t }
            }
            // dateTime with an offset: scale, then time, date and offset minutes (UTC).
            0x7B | 0x7E | 0x7F | 0x7D => {
                let scale = if token == 0x7F { 0 } else { self.take(1)?[0] };
                let tb = super::record::time_bytes(scale);
                let time = if token == 0x7F { 0 } else { int(self.take(tb)?) };
                let days = if token == 0x7D { 0 } else { int(self.take(3)?) };
                let offset = if token == 0x7B { int(self.take(2)?) as i16 as i64 } else { 0 };
                let ns = time * 10i64.pow(9 - scale as u32);
                let local = days as i128 * super::values::DAY + ns as i128 + offset as i128 * 60_000_000_000;
                let (d, t) = (local.div_euclid(super::values::DAY) as i64, local.rem_euclid(super::values::DAY) as i64);
                let date = super::text::odbc(&crate::data::value::Value::Date(d), super::values::xtype::DATE, 0);
                let secs = t / 1_000_000_000;
                let mut clock = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
                let frac = format!("{:09}", t % 1_000_000_000);
                let frac = frac.trim_end_matches('0');
                if !frac.is_empty() {
                    clock.push('.');
                    clock.push_str(frac);
                }
                let zone = match (token, offset) {
                    (0x7B, 0) => "Z".to_string(),
                    (0x7B, o) => format!("{}{:02}:{:02}", if o < 0 { '-' } else { '+' }, o.abs() / 60, o.abs() % 60),
                    _ => String::new(),
                };
                match token {
                    0x7F => format!("{date}{zone}"),
                    0x7D => format!("{clock}{zone}"),
                    _ => format!("{date}T{clock}{zone}"),
                }
            }
            _ => return Ok(None),
        }))
    }

    fn text(&mut self) -> Result<String, String> {
        let n = self.number()? * 2;
        let s = self.b.get(self.at..self.at + n).ok_or("xml value ends early")?;
        self.at += n;
        Ok(utf16(s))
    }
}

fn escape(s: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\r' => out.push_str("&#xD;"),
            '\n' if attribute => out.push_str("&#xA;"),
            '\t' if attribute => out.push_str("&#x9;"),
            c => out.push(c),
        }
    }
    out
}

/// xs:double as SQL Server writes it: the shortest form, an exponent for large and small values.
fn double_text(f: f64) -> String {
    if f == 0.0 || (1e-6..1e15).contains(&f.abs()) { format!("{f}") } else { format!("{f:E}").replace("E", "E") }
}

pub fn decode(b: &[u8]) -> Result<String, String> {
    if b.len() < 5 || b[0] != 0xDF || b[1] != 0xFF {
        return Err("not a stored xml value".into());
    }
    let mut r = Reader { b, at: 5 };
    let mut names: Vec<String> = vec![String::new()];
    let mut qnames: Vec<String> = vec![String::new()];
    let mut out = String::new();
    // Elements open, and whether the start tag is still unfinished.
    let mut open: Vec<String> = Vec::new();
    let mut pending = false;
    let close_start = |out: &mut String, pending: &mut bool| {
        if *pending {
            out.push('>');
            *pending = false;
        }
    };
    while r.at < b.len() {
        match r.byte()? {
            NAME => names.push(r.text()?),
            QNAME => {
                let (_ns, prefix, local) = (r.number()?, r.number()?, r.number()?);
                let name = |i: usize| names.get(i).cloned().unwrap_or_default();
                qnames.push(match (prefix, local) {
                    (0, l) => name(l),
                    (p, 0) => name(p),
                    (p, l) => format!("{}:{}", name(p), name(l)),
                });
            }
            ELEMENT => {
                close_start(&mut out, &mut pending);
                let q = qnames.get(r.number()?).cloned().unwrap_or_default();
                out.push('<');
                out.push_str(&q);
                open.push(q);
                pending = true;
            }
            ATTRIBUTE => {
                let q = qnames.get(r.number()?).cloned().unwrap_or_default();
                let mut value = String::new();
                while let Some(&t) = r.b.get(r.at) {
                    if t == EXTENSION {
                        r.at += 1;
                        let n = r.number()?;
                        r.take(n)?;
                        continue;
                    }
                    let at = r.at;
                    r.at += 1;
                    match r.atomic(t)? {
                        Some(v) => value.push_str(&v),
                        None => {
                            r.at = at;
                            break;
                        }
                    }
                }
                out.push_str(&format!(" {q}=\"{}\"", escape(&value, true)));
            }
            END_ATTRIBUTES => {}
            END_ELEMENT => {
                let q = open.pop().unwrap_or_default();
                if pending {
                    out.push_str("/>");
                    pending = false;
                } else {
                    out.push_str(&format!("</{q}>"));
                }
            }
            EXTENSION => {
                let n = r.number()?;
                r.take(n)?;
            }
            COMMENT => {
                close_start(&mut out, &mut pending);
                let t = r.text()?;
                out.push_str(&format!("<!--{t}-->"));
            }
            PI => {
                close_start(&mut out, &mut pending);
                let target = names.get(r.number()?).cloned().unwrap_or_default();
                let t = r.text()?;
                out.push_str(&format!("<?{target}{}{t}?>", if t.is_empty() { "" } else { " " }));
            }
            XML_DECLARATION => {
                // Version, encoding and standalone: not part of the value's text.
                r.text()?;
                r.text()?;
                r.byte()?;
            }
            t => match r.atomic(t)? {
                // An empty text leaves the element empty (`<s/>`).
                Some(v) if v.is_empty() => {}
                Some(v) => {
                    close_start(&mut out, &mut pending);
                    out.push_str(&escape(&v, false));
                }
                None => return Err(format!("unsupported token 0x{t:02X} in xml value")),
            },
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn stored_xml_as_text() {
        let a = hex("DFFF01B004F0016100EF000001F801F0017800EF000002F60211013100F5F0016200EF000003F803110274002600F7F3016300F00270006900F404016400F803F7F7");
        assert_eq!(decode(&a).unwrap(), r#"<a x="1"><b>t&amp;</b><!--c--><?pi d?><b/></a>"#);
        let r = hex("DFFF01B004F0017200F0017500EF020001F801F00578006D006C006E007300EF000300F60211017500F00778006D006C006E0073003A007000EF000400F60311017600F5F0017100F0017600F0017000EF060705F804F0017A00EF060708F60511013200F511017800F71106630020007400650078007400F7");
        assert_eq!(decode(&r).unwrap(), r#"<r xmlns="u" xmlns:p="v"><p:q p:z="2">x</p:q>c text</r>"#);
        assert_eq!(decode(&hex("DFFF01B004110374006F007000F0016100EF000001F801F711047400610069006C00")).unwrap(), "top<a/>tail");
        assert_eq!(decode(&hex("DFFF01B004")).unwrap(), "");
    }
}
