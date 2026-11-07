//! Tables in the backup: which pages an allocation unit owns (its IAM chain, checked against
//! the free-space pages), the system base tables that describe every rowset, and the rows of
//! any table.

use std::collections::HashMap;

use super::mtf::u16_at;
use super::pages::{self, Header, PageId, PageStore};
use super::record::{Col, Field, RECORD_FORWARDED, RECORD_PRIMARY, Record};
use super::values;
use crate::data::value::Value;

/// Allocation unit types.
pub const IN_ROW: u8 = 1;

const PFS_INTERVAL: u32 = 8088;

/// Pages of `alloc_unit` reached from its first IAM page, in file order.
pub fn unit_pages(store: &PageStore, first_iam: PageId, alloc_unit: u64, kinds: &[u8]) -> Vec<PageId> {
    let mut candidates: Vec<PageId> = Vec::new();
    let mut iam = first_iam;
    let mut seen = 0;
    while !iam.is_null() && seen < 100_000 {
        seen += 1;
        let Some(p) = store.page(iam) else { break };
        let h = pages::header(&p);
        if h.kind != 10 {
            break;
        }
        let start = PageId::at(&p, 136);
        for i in 0..8 {
            let s = PageId::at(&p, 142 + 6 * i);
            if !s.is_null() {
                candidates.push(s);
            }
        }
        if let Some(bitmap) = pages::slot(&p, 1) {
            let end = (u16_at(bitmap, 2) as usize).min(bitmap.len());
            for (byte_no, &b) in bitmap.get(4..end).unwrap_or_default().iter().enumerate() {
                if b == 0 {
                    continue;
                }
                for bit in 0..8 {
                    if b & (1 << bit) != 0 {
                        let first = start.page + (byte_no as u32 * 8 + bit) * 8;
                        candidates.extend((first..first + 8).map(|page| PageId { file: start.file, page }));
                    }
                }
            }
        }
        iam = h.next;
    }
    candidates.sort();
    candidates.dedup();
    candidates
        .into_iter()
        .filter(|&id| allocated(store, id))
        .filter(|&id| {
            store.page(id).is_some_and(|p| {
                let h = pages::header(&p);
                h.alloc_unit == alloc_unit && kinds.contains(&h.kind)
            })
        })
        .collect()
}

/// Whether the free-space page covering `id` marks it allocated (assumed when it is missing).
fn allocated(store: &PageStore, id: PageId) -> bool {
    let interval = id.page / PFS_INTERVAL;
    let pfs = PageId { file: id.file, page: if interval == 0 { 1 } else { interval * PFS_INTERVAL } };
    match store.page(pfs) {
        Some(p) if pages::header(&p).kind == 11 => p.get(100 + (id.page % PFS_INTERVAL) as usize).is_none_or(|b| b & 0x40 != 0),
        _ => true,
    }
}

/// The live rows of the data pages: primary and forwarded records, not ghosts or stubs.
pub fn records(store: &PageStore, data_pages: &[PageId], mut each: impl FnMut(&Record)) {
    for &id in data_pages {
        let Some(p) = store.page(id) else { continue };
        let h: Header = pages::header(&p);
        for s in 0..h.slots {
            let Some(bytes) = pages::slot(&p, s) else { continue };
            if let Some(r) = Record::parse(bytes)
                && (r.kind == RECORD_PRIMARY || r.kind == RECORD_FORWARDED)
            {
                each(&r);
            }
        }
    }
}

/// A rowset's allocation unit: type, id and first IAM page.
#[derive(Clone, Debug)]
pub struct Unit {
    pub kind: u8,
    pub auid: u64,
    pub first_iam: PageId,
}

/// One row of a system table, by column name.
#[derive(Clone, Debug, Default)]
pub struct SysRow(pub HashMap<String, Value>);

impl SysRow {
    pub fn get(&self, name: &str) -> &Value {
        self.0.get(name).unwrap_or(&Value::Null)
    }

    pub fn int(&self, name: &str) -> i64 {
        match self.get(name) {
            Value::Int(v) => *v,
            Value::Bool(b) => *b as i64,
            Value::Decimal(v, 0) => *v as i64,
            Value::Variant(v) => v.text.parse().unwrap_or(0),
            _ => 0,
        }
    }

    pub fn str(&self, name: &str) -> String {
        match self.get(name) {
            Value::Str(s) => s.clone(),
            _ => String::new(),
        }
    }

    pub fn bytes(&self, name: &str) -> Vec<u8> {
        match self.get(name) {
            Value::Bytes(b) => b.clone(),
            _ => Vec::new(),
        }
    }
}

/// A physical column of a rowset: rscolid, offset, null bit, bit position, status.
pub type RsCol = (i32, i16, u16, u8, i32);

/// The system base tables, read once.
pub struct Catalog<'a> {
    pub store: &'a PageStore,
    /// The database's default collation id (from the boot page).
    pub collation: u32,
    /// rowsetid → allocation units.
    pub units: HashMap<u64, Vec<Unit>>,
    /// object id → its rowsets of the heap or clustered index: (index id, partition number, rowsetid).
    pub rowsets: HashMap<i32, Vec<(i32, i32, u64)>>,
    pub rowset_rows: Vec<SysRow>,
    /// rowsetid → physical columns: (rscolid, offset, null bit, bit position, status).
    pub rscols: HashMap<u64, Vec<RsCol>>,
    /// object id → its columns (syscolpars rows with number 0).
    pub colpars: HashMap<i32, Vec<SysRow>>,
    /// object id → its parameters (syscolpars rows with number > 0).
    pub parameters: HashMap<i32, Vec<SysRow>>,
    /// sysseobjvalues: (rowsetid, hobt column) → value (the defaults of added columns).
    pub seobjvalues: HashMap<(i64, i64), Vec<u8>>,
    /// System table name → object id (schema sys).
    pub system_tables: HashMap<String, i32>,
}

fn fixed(id: i32, offset: i16, length: i16, xtype: u8, name: &str) -> Col {
    Col { id, name: name.into(), xtype, length, offset, null_bit: id as u16, ..Default::default() }
}

fn var(id: i32, index: i16, length: i16, xtype: u8, name: &str) -> Col {
    Col { id, name: name.into(), xtype, length, offset: -index, null_bit: id as u16, ..Default::default() }
}

/// The base tables whose layout is needed before sysrscols can be read.
fn sysallocunits() -> Vec<Col> {
    vec![
        fixed(1, 4, 8, 127, "auid"),
        fixed(2, 12, 1, 48, "type"),
        fixed(3, 13, 8, 127, "ownerid"),
        fixed(4, 21, 4, 56, "status"),
        fixed(5, 25, 2, 52, "fgid"),
        fixed(6, 27, 6, 173, "pgfirst"),
        fixed(7, 33, 6, 173, "pgroot"),
        fixed(8, 39, 6, 173, "pgfirstiam"),
    ]
}

fn sysrowsets() -> Vec<Col> {
    vec![
        fixed(1, 4, 8, 127, "rowsetid"),
        fixed(2, 12, 1, 48, "ownertype"),
        fixed(3, 13, 4, 56, "idmajor"),
        fixed(4, 17, 4, 56, "idminor"),
        fixed(5, 21, 4, 56, "numpart"),
        fixed(6, 25, 4, 56, "status"),
        fixed(7, 29, 2, 52, "fgidfs"),
        fixed(8, 31, 8, 127, "rcrows"),
        fixed(9, 39, 1, 48, "cmprlevel"),
        fixed(10, 40, 1, 48, "fillfact"),
    ]
}

fn sysrscols() -> Vec<Col> {
    vec![
        fixed(1, 4, 8, 127, "rsid"),
        fixed(2, 12, 4, 56, "rscolid"),
        fixed(3, 16, 4, 56, "hbcolid"),
        fixed(5, 28, 4, 56, "ti"),
        fixed(6, 32, 4, 56, "cid"),
        fixed(7, 36, 2, 52, "ordkey"),
        fixed(9, 40, 4, 56, "status"),
        fixed(10, 44, 4, 56, "offset"),
        fixed(11, 48, 4, 56, "nullbit"),
        fixed(12, 52, 2, 52, "bitpos"),
    ]
}

fn syscolpars() -> Vec<Col> {
    vec![
        fixed(1, 4, 4, 56, "id"),
        fixed(2, 8, 2, 52, "number"),
        fixed(3, 10, 4, 56, "colid"),
        fixed(5, 14, 1, 48, "xtype"),
        fixed(6, 15, 4, 56, "utype"),
        fixed(7, 19, 2, 52, "length"),
        fixed(8, 21, 1, 48, "prec"),
        fixed(9, 22, 1, 48, "scale"),
        fixed(10, 23, 4, 56, "collationid"),
        fixed(11, 27, 4, 56, "status"),
        fixed(12, 31, 2, 52, "maxinrow"),
        fixed(13, 33, 4, 56, "xmlns"),
        fixed(14, 37, 4, 56, "dflt"),
        fixed(15, 41, 4, 56, "chk"),
        var(4, 1, 256, 231, "name"),
        var(16, 2, 64, 165, "idtval"),
    ]
}

fn sysseobjvalues() -> Vec<Col> {
    vec![
        fixed(1, 4, 1, 48, "valclass"),
        fixed(2, 5, 8, 127, "id"),
        fixed(3, 13, 8, 127, "subid"),
        fixed(4, 21, 4, 56, "valnum"),
        var(5, 1, 8016, 98, "value"),
        var(6, 2, -1, 165, "imageval"),
    ]
}

const fn auid(object: u64, index: u64) -> u64 {
    (index << 48) | (object << 16)
}

impl<'a> Catalog<'a> {
    pub fn open(store: &'a PageStore, collation: u32) -> Result<Catalog<'a>, String> {
        let mut cat = Catalog {
            store,
            collation,
            units: HashMap::new(),
            rowsets: HashMap::new(),
            rowset_rows: Vec::new(),
            rscols: HashMap::new(),
            colpars: HashMap::new(),
            parameters: HashMap::new(),
            seobjvalues: HashMap::new(),
            system_tables: HashMap::new(),
        };
        // sysallocunits: its first IAM page is found among the first pages of file 1.
        let au = auid(7, 0);
        let iam = (0..1000u32)
            .map(|page| PageId { file: 1, page })
            .find(|&id| {
                store.page(id).is_some_and(|p| {
                    let h = pages::header(&p);
                    h.kind == 10 && h.alloc_unit == au
                })
            })
            .ok_or("the allocation units table was not found in the backup")?;
        for r in cat.raw_rows(&sysallocunits(), iam, au) {
            let unit = Unit { kind: r.int("type") as u8, auid: r.int("auid") as u64, first_iam: PageId::at(&r.bytes("pgfirstiam"), 0) };
            cat.units.entry(r.int("ownerid") as u64).or_default().push(unit);
        }
        let first_iam = |cat: &Catalog, rowset: u64| -> Result<(PageId, u64), String> {
            cat.units
                .get(&rowset)
                .and_then(|u| u.iter().find(|u| u.kind == IN_ROW))
                .map(|u| (u.first_iam, u.auid))
                .ok_or_else(|| format!("system rowset {rowset} was not found in the backup"))
        };
        let (iam, au) = first_iam(&cat, auid(5, 0))?;
        for r in cat.raw_rows(&sysrowsets(), iam, au) {
            let id = r.int("rowsetid") as u64;
            let (major, minor) = (r.int("idmajor") as i32, r.int("idminor") as i32);
            if minor <= 1 && r.int("ownertype") == 1 {
                cat.rowsets.entry(major).or_default().push((minor, r.int("numpart") as i32, id));
            }
            cat.rowset_rows.push(r);
        }
        let (iam, au) = first_iam(&cat, auid(3, 0))?;
        for r in cat.raw_rows(&sysrscols(), iam, au) {
            let entry = (r.int("rscolid") as i32, r.int("offset") as i16, r.int("nullbit") as u16, r.int("bitpos") as u8, r.int("status") as i32);
            cat.rscols.entry(r.int("rsid") as u64).or_default().push(entry);
        }
        let (iam, au) = first_iam(&cat, auid(41, 1))?;
        for r in cat.raw_rows(&syscolpars(), iam, au) {
            if r.int("number") == 0 {
                cat.colpars.entry(r.int("id") as i32).or_default().push(r);
            } else {
                cat.parameters.entry(r.int("id") as i32).or_default().push(r);
            }
        }
        if let Ok((iam, au)) = first_iam(&cat, auid(9, 1)) {
            for r in cat.raw_rows(&sysseobjvalues(), iam, au) {
                cat.seobjvalues.insert((r.int("id"), r.int("subid")), r.bytes("value_raw"));
            }
        }
        // sysschobjs is read through the catalog now that columns can be laid out.
        for r in cat.table_rows(34) {
            if r.int("nsid") == 4 && r.str("type").trim() == "S" {
                cat.system_tables.insert(r.str("name"), r.int("id") as i32);
            }
        }
        Ok(cat)
    }

    fn raw_rows(&self, cols: &[Col], iam: PageId, au: u64) -> Vec<SysRow> {
        let data = unit_pages(self.store, iam, au, &[1]);
        let mut out = Vec::new();
        records(self.store, &data, |r| out.push(self.sys_row(cols, r)));
        out
    }

    fn sys_row(&self, cols: &[Col], r: &Record) -> SysRow {
        let mut row = HashMap::new();
        for c in cols {
            let v = match r.field(c) {
                Field::Null => Value::Null,
                Field::Missing => match &c.default {
                    Some(d) => values::decode_variant(d, self.collation).unwrap_or(Value::Null),
                    None => Value::Null,
                },
                Field::Bytes(b) => {
                    if c.xtype == 98 {
                        row.insert(format!("{}_raw", c.name), Value::Bytes(b.to_vec()));
                    }
                    values::decode(self.store, c, b, false, self.collation).unwrap_or(Value::Null)
                }
                Field::Complex(b) => values::decode(self.store, c, b, true, self.collation).unwrap_or(Value::Null),
            };
            row.insert(c.name.clone(), v);
        }
        SysRow(row)
    }

    /// The physical columns of a heap or clustered index rowset of `object`, named and typed
    /// from syscolpars.
    pub fn columns(&self, object: i32, rowset: u64) -> Vec<Col> {
        let pars: HashMap<i64, &SysRow> = self.colpars.get(&object).map(|v| v.iter().map(|r| (r.int("colid"), r)).collect()).unwrap_or_default();
        let mut cols = Vec::new();
        for &(rscolid, offset, null_bit, bit_pos, _status) in self.rscols.get(&rowset).map(Vec::as_slice).unwrap_or_default() {
            let Some(p) = pars.get(&(rscolid as i64)) else { continue };
            cols.push(Col {
                id: rscolid,
                name: p.str("name"),
                xtype: p.int("xtype") as u8,
                length: p.int("length") as i16,
                prec: p.int("prec") as u8,
                scale: p.int("scale") as u8,
                collation: p.int("collationid") as u32,
                offset,
                null_bit,
                bit_pos,
                default: self.seobjvalues.get(&(rowset as i64, rscolid as i64)).cloned(),
            });
        }
        cols.sort_by_key(|c| c.id);
        cols
    }

    /// The partitions of `object`'s heap or clustered index: (partition number, rowsetid).
    pub fn partitions(&self, object: i32) -> Vec<(i32, u64)> {
        let mut v: Vec<(i32, u64)> = self.rowsets.get(&object).map(|r| r.iter().map(|&(_, part, id)| (part, id)).collect()).unwrap_or_default();
        v.sort();
        v
    }

    /// The in-row data pages of a rowset.
    pub fn data_pages(&self, rowset: u64) -> Vec<PageId> {
        match self.units.get(&rowset).and_then(|u| u.iter().find(|u| u.kind == IN_ROW)) {
            Some(unit) => unit_pages(self.store, unit.first_iam, unit.auid, &[1]),
            None => Vec::new(),
        }
    }

    /// Every row of a table (all partitions), by column name.
    pub fn table_rows(&self, object: i32) -> Vec<SysRow> {
        let mut out = Vec::new();
        for (_, rowset) in self.partitions(object) {
            let cols = self.columns(object, rowset);
            records(self.store, &self.data_pages(rowset), |r| out.push(self.sys_row(&cols, r)));
        }
        out
    }

    /// Rows of a system table by name; empty when the backup has no such table.
    pub fn system(&self, name: &str) -> Vec<SysRow> {
        match self.system_tables.get(name) {
            Some(&id) => self.table_rows(id),
            None => Vec::new(),
        }
    }
}
