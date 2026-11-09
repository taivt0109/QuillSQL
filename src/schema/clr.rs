//! Enough of an assembly's PE/CLI metadata (ECMA-335 §II.24–25) to tell whether a new build
//! changes what SQL Server binds to: its public surface — types with their flags, methods with
//! their parameter types, and fields. The classic compare tool alters an assembly in place when the surface
//! is the same and drops and creates it otherwise.

/// One type of the surface.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TypeSurface {
    pub name: String,
    pub visibility: u32,
    pub before_field_init: bool,
    /// (name, member access, parameter types), sorted.
    pub methods: Vec<(String, u16, Vec<String>)>,
    /// (name, member access), sorted.
    pub fields: Vec<(String, u16)>,
}

/// The module version id and the types, sorted by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    pub mvid: [u8; 16],
    pub types: Vec<TypeSurface>,
}

/// Decodes an upper- or lower-case hex string.
pub fn from_hex(hex: &str) -> Option<Vec<u8>> {
    let b = hex.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let nib = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    b.chunks(2).map(|p| Some(nib(p[0])? << 4 | nib(p[1])?)).collect()
}

struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn u8(&self, at: usize) -> Option<u8> {
        self.0.get(at).copied()
    }
    fn u16(&self, at: usize) -> Option<u16> {
        Some(u16::from_le_bytes(self.0.get(at..at + 2)?.try_into().ok()?))
    }
    fn u32(&self, at: usize) -> Option<u32> {
        Some(u32::from_le_bytes(self.0.get(at..at + 4)?.try_into().ok()?))
    }
    /// A 2- or 4-byte index.
    fn idx(&self, at: usize, wide: bool) -> Option<u32> {
        if wide { self.u32(at) } else { self.u16(at).map(u32::from) }
    }
}

/// File offset of an RVA through the section table.
fn offset(pe: &Bytes, sections: usize, count: usize, rva: u32) -> Option<usize> {
    for k in 0..count {
        let s = sections + k * 40;
        let (vsize, va, raw_size, raw) = (pe.u32(s + 8)?, pe.u32(s + 12)?, pe.u32(s + 16)?, pe.u32(s + 20)?);
        if rva >= va && rva < va + vsize.max(raw_size) {
            return Some((rva - va + raw) as usize);
        }
    }
    None
}

/// A compressed unsigned integer of a signature blob; advances `at`.
fn compressed(b: &[u8], at: &mut usize) -> Option<u32> {
    let x = *b.get(*at)?;
    let (v, n) = if x & 0x80 == 0 {
        (u32::from(x), 1)
    } else if x & 0xC0 == 0x80 {
        ((u32::from(x & 0x3F) << 8) | u32::from(*b.get(*at + 1)?), 2)
    } else {
        ((u32::from(x & 0x1F) << 24) | (u32::from(*b.get(*at + 1)?) << 16) | (u32::from(*b.get(*at + 2)?) << 8) | u32::from(*b.get(*at + 3)?), 4)
    };
    *at += n;
    Some(v)
}

struct Meta<'a> {
    strings: &'a [u8],
    blobs: &'a [u8],
    type_names: Vec<String>,
    ref_names: Vec<String>,
}

impl Meta<'_> {
    fn string(&self, at: u32) -> String {
        let s = self.strings.get(at as usize..).unwrap_or_default();
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        String::from_utf8_lossy(&s[..end]).into_owned()
    }

    fn blob(&self, at: u32) -> &[u8] {
        let mut p = at as usize;
        let Some(len) = compressed(self.blobs, &mut p) else { return &[] };
        self.blobs.get(p..p + len as usize).unwrap_or_default()
    }

    /// A TypeDefOrRefOrSpecEncoded token's name.
    fn token(&self, v: u32) -> String {
        let row = (v >> 2) as usize;
        match v & 3 {
            0 => self.type_names.get(row.wrapping_sub(1)).cloned(),
            1 => self.ref_names.get(row.wrapping_sub(1)).cloned(),
            _ => Some(format!("spec{row}")),
        }
        .unwrap_or_default()
    }

    /// One type of a signature as text; advances `at`.
    fn sig_type(&self, b: &[u8], at: &mut usize, depth: u32) -> Option<String> {
        if depth > 32 {
            return None;
        }
        let e = *b.get(*at)?;
        *at += 1;
        Some(match e {
            0x01 => "void".into(),
            0x02 => "bool".into(),
            0x03 => "char".into(),
            0x04 => "i1".into(),
            0x05 => "u1".into(),
            0x06 => "i2".into(),
            0x07 => "u2".into(),
            0x08 => "i4".into(),
            0x09 => "u4".into(),
            0x0A => "i8".into(),
            0x0B => "u8".into(),
            0x0C => "r4".into(),
            0x0D => "r8".into(),
            0x0E => "string".into(),
            0x0F => format!("{}*", self.sig_type(b, at, depth + 1)?),
            0x10 => format!("{}&", self.sig_type(b, at, depth + 1)?),
            0x11 | 0x12 => self.token(compressed(b, at)?),
            0x13 => format!("!{}", compressed(b, at)?),
            0x1E => format!("!!{}", compressed(b, at)?),
            0x14 => {
                let t = self.sig_type(b, at, depth + 1)?;
                let rank = compressed(b, at)?;
                for _ in 0..2 {
                    let n = compressed(b, at)?;
                    for _ in 0..n {
                        compressed(b, at)?;
                    }
                }
                format!("{t}[{rank}]")
            }
            0x15 => {
                *at += 1;
                let t = self.token(compressed(b, at)?);
                let n = compressed(b, at)?;
                let args: Option<Vec<String>> = (0..n).map(|_| self.sig_type(b, at, depth + 1)).collect();
                format!("{t}<{}>", args?.join(","))
            }
            0x16 => "typedref".into(),
            0x18 => "native int".into(),
            0x19 => "native uint".into(),
            0x1C => "object".into(),
            0x1D => format!("{}[]", self.sig_type(b, at, depth + 1)?),
            0x1F | 0x20 => {
                let m = self.token(compressed(b, at)?);
                format!("{} mod {m}", self.sig_type(b, at, depth + 1)?)
            }
            0x45 => self.sig_type(b, at, depth + 1)?,
            0x1B => {
                let s = self.method_params(&b[*at..])?;
                // A function pointer's length is not known up front; keep the rest as one type.
                *at = b.len();
                format!("fnptr({})", s.join(","))
            }
            _ => return None,
        })
    }

    /// The parameter types of a method signature.
    fn method_params(&self, b: &[u8]) -> Option<Vec<String>> {
        let mut at = 0;
        let conv = *b.first()?;
        at += 1;
        if conv & 0x10 != 0 {
            compressed(b, &mut at)?;
        }
        let n = compressed(b, &mut at)?;
        self.sig_type(b, &mut at, 0)?;
        (0..n).map(|_| self.sig_type(b, &mut at, 0)).collect()
    }
}

/// The public surface of an assembly image; None when it can't be read as one.
pub fn surface(image: &[u8]) -> Option<Surface> {
    let pe = Bytes(image);
    if image.len() < 96 || pe.u16(0)? != 0x5A4D {
        return None;
    }
    let nt = pe.u32(0x3C)? as usize;
    if pe.u32(nt)? != 0x0000_4550 {
        return None;
    }
    let coff = nt + 4;
    let sections_count = pe.u16(coff + 2)? as usize;
    let opt = coff + 20;
    let opt_size = pe.u16(coff + 16)? as usize;
    let dirs = match pe.u16(opt)? {
        0x10B => opt + 96,
        0x20B => opt + 112,
        _ => return None,
    };
    let sections = opt + opt_size;
    let cli = offset(&pe, sections, sections_count, pe.u32(dirs + 14 * 8)?)?;
    let root = offset(&pe, sections, sections_count, pe.u32(cli + 8)?)?;
    if pe.u32(root)? != 0x424A_5342 {
        return None;
    }
    let version_len = pe.u32(root + 12)? as usize;
    let mut p = root + 16 + version_len + 2;
    let streams = pe.u16(p)? as usize;
    p += 2;
    let (mut tables, mut strings, mut blobs, mut guids) = (None, None, None, None);
    for _ in 0..streams {
        let (off, size) = (pe.u32(p)? as usize, pe.u32(p + 4)? as usize);
        let name_at = p + 8;
        let end = image.get(name_at..)?.iter().position(|&c| c == 0)?;
        let name = std::str::from_utf8(&image[name_at..name_at + end]).ok()?;
        let range = image.get(root + off..root + off + size)?;
        match name {
            "#~" => tables = Some(range),
            "#Strings" => strings = Some(range),
            "#Blob" => blobs = Some(range),
            "#GUID" => guids = Some(range),
            _ => {}
        }
        p = name_at + (end + 4) / 4 * 4;
    }
    let (t, strings, blobs, guids) = (Bytes(tables?), strings?, blobs?, guids?);
    let heaps = t.u8(6)?;
    let (wide_str, wide_guid, wide_blob) = (heaps & 1 != 0, heaps & 2 != 0, heaps & 4 != 0);
    let valid = u64::from(t.u32(8)?) | (u64::from(t.u32(12)?) << 32);
    let mut rows = [0u32; 64];
    let mut q = 24;
    for (k, r) in rows.iter_mut().enumerate() {
        if valid >> k & 1 == 1 {
            *r = t.u32(q)?;
            q += 4;
        }
    }
    // Field and method pointer tables only appear in unoptimised metadata.
    if rows[3] != 0 || rows[5] != 0 {
        return None;
    }
    let wide = |table: usize| rows[table] >= 1 << 16;
    let coded = |tables: &[usize], bits: u32| tables.iter().any(|&k| rows[k] >= 1 << (16 - bits));
    let (s, g, b) = (if wide_str { 4 } else { 2 }, if wide_guid { 4 } else { 2 }, if wide_blob { 4 } else { 2 });
    let resolution = if coded(&[0x00, 0x1A, 0x23, 0x01], 2) { 4 } else { 2 };
    let type_ref = if coded(&[0x02, 0x01, 0x1B], 2) { 4 } else { 2 };
    let (field_i, method_i, param_i) = (if wide(4) { 4 } else { 2 }, if wide(6) { 4 } else { 2 }, if wide(8) { 4 } else { 2 });
    let module_row = 2 + s + 3 * g;
    let typeref_row = resolution + 2 * s;
    let typedef_row = 4 + 2 * s + type_ref + field_i + method_i;
    let field_row = 2 + s + b;
    let method_row = 4 + 2 + 2 + s + b + param_i;

    let module = q;
    let typeref = module + rows[0] as usize * module_row;
    let typedef = typeref + rows[1] as usize * typeref_row;
    let field = typedef + rows[2] as usize * typedef_row;
    let method = field + rows[4] as usize * field_row;
    let mvid_index = t.idx(module + 2 + s, g == 4)? as usize;
    let mvid: [u8; 16] = guids.get((mvid_index.checked_sub(1)?) * 16..mvid_index * 16)?.try_into().ok()?;

    let mut meta = Meta { strings, blobs, type_names: Vec::new(), ref_names: Vec::new() };
    let full = |m: &Meta, ns: u32, n: u32| {
        let (ns, n) = (m.string(ns), m.string(n));
        if ns.is_empty() { n } else { format!("{ns}.{n}") }
    };
    for k in 0..rows[1] as usize {
        let r = typeref + k * typeref_row;
        let name = full(&meta, t.idx(r + resolution + s, s == 4)?, t.idx(r + resolution, s == 4)?);
        meta.ref_names.push(name);
    }
    struct Def {
        flags: u32,
        name: String,
        fields: usize,
        methods: usize,
    }
    let mut defs = Vec::new();
    for k in 0..rows[2] as usize {
        let r = typedef + k * typedef_row;
        let name = full(&meta, t.idx(r + 4 + s, s == 4)?, t.idx(r + 4, s == 4)?);
        let fl = t.idx(r + 4 + 2 * s + type_ref, field_i == 4)? as usize;
        let ml = t.idx(r + 4 + 2 * s + type_ref + field_i, method_i == 4)? as usize;
        defs.push(Def { flags: t.u32(r)?, name, fields: fl, methods: ml });
    }
    meta.type_names = defs.iter().map(|d| d.name.clone()).collect();
    let mut types = Vec::new();
    for (k, d) in defs.iter().enumerate() {
        let next = defs.get(k + 1);
        let field_end = next.map(|n| n.fields).unwrap_or(rows[4] as usize + 1);
        let method_end = next.map(|n| n.methods).unwrap_or(rows[6] as usize + 1);
        let mut fields = Vec::new();
        for f in d.fields..field_end.max(d.fields) {
            let r = field + (f - 1) * field_row;
            fields.push((meta.string(t.idx(r + 2, s == 4)?), t.u16(r)? & 7));
        }
        let mut methods = Vec::new();
        for m in d.methods..method_end.max(d.methods) {
            let r = method + (m - 1) * method_row;
            let name = meta.string(t.idx(r + 8, s == 4)?);
            let params = meta.method_params(meta.blob(t.idx(r + 8 + s, b == 4)?))?;
            methods.push((name, t.u16(r + 6)? & 7, params));
        }
        fields.sort();
        methods.sort();
        types.push(TypeSurface { name: d.name.clone(), visibility: d.flags & 7, before_field_init: d.flags & 0x0010_0000 != 0, methods, fields });
    }
    types.sort();
    Some(Surface { mvid, types })
}

/// Whether a new build of an assembly can replace the old one with ALTER ASSEMBLY: the same
/// module, or the same public surface.
pub fn alterable(new: &[u8], old: &[u8]) -> bool {
    match (surface(new), surface(old)) {
        (Some(a), Some(b)) => a.mvid == b.mvid || a.types == b.types,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(hex: &str) -> Vec<u8> {
        from_hex(hex.trim()).unwrap()
    }

    #[test]
    fn reads_the_surface() {
        let s = surface(&image(include_str!("../../tests/fixtures/objects/clr.hex"))).unwrap();
        let names: Vec<&str> = s.types.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"QuillFunctions") && names.contains(&"Point") && names.contains(&"CountAll"), "{names:?}");
        let f = s.types.iter().find(|t| t.name == "QuillFunctions").unwrap();
        let add_one = f.methods.iter().find(|m| m.0 == "AddOne").unwrap();
        assert_eq!(add_one.1, 6, "public");
        assert_eq!(add_one.2.len(), 1);
        assert!(add_one.2[0].ends_with("SqlInt32"), "{add_one:?}");
    }

    #[test]
    fn same_surface_alters_a_new_one_rebuilds() {
        let (a, b, c) = (
            image(include_str!("../../tests/fixtures/objects/clr.hex")),
            image(include_str!("../../tests/fixtures/objects/clr2.hex")),
            image(include_str!("../../tests/fixtures/objects/clr3.hex")),
        );
        assert!(alterable(&b, &a));
        assert!(!alterable(&c, &a));
        assert!(!alterable(&[0; 200], &a));
    }
}
