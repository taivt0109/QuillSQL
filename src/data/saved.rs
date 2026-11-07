//! A comparison saved to one file and opened again later: every table's result and the rows it
//! stored. Layout: a magic line, the length of a JSON header (little-endian u64), the header,
//! then each table's row file in table order.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::compare::{DataComparison, SideKind, TableResult};
use super::mapping::TablePair;
use super::store::Session;
use crate::catalog::ObjectName;

const MAGIC: &[u8] = b"QuillSQL data comparison 1\n";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    /// Whatever the caller keeps with the comparison (project, target schema).
    extra: serde_json::Value,
    only_in_1: Vec<ObjectName>,
    only_in_2: Vec<ObjectName>,
    ddl_triggers: Vec<String>,
    tables: Vec<SavedTable>,
    #[serde(default)]
    excluded: Vec<TablePair>,
    #[serde(default)]
    kinds: [SideKind; 2],
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedTable {
    pair: TablePair,
    counts: [u64; 4],
    offsets: [Vec<u64>; 4],
    error: Option<String>,
    by_checksum: bool,
    repeated_keys: bool,
    identity_current: (Option<i64>, Option<i64>),
    column_differences: Vec<u64>,
    /// Bytes of the row file that follow in the saved file.
    size: u64,
}

pub fn save(result: &DataComparison, extra: serde_json::Value, path: &Path) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let size = |t: &TableResult| std::fs::metadata(&t.path).map(|m| m.len()).unwrap_or(0);
    let header = Header {
        extra,
        only_in_1: result.only_in_1.clone(),
        only_in_2: result.only_in_2.clone(),
        ddl_triggers: result.ddl_triggers.clone(),
        excluded: result.excluded.clone(),
        kinds: result.kinds,
        tables: result
            .tables
            .iter()
            .map(|t| SavedTable {
                pair: t.pair.clone(),
                counts: t.counts,
                offsets: t.offsets.clone(),
                error: t.error.clone(),
                by_checksum: t.by_checksum,
                repeated_keys: t.repeated_keys,
                identity_current: (t.identity_current.0.map(|v| v as i64), t.identity_current.1.map(|v| v as i64)),
                column_differences: t.column_differences.clone(),
                size: size(t),
            })
            .collect(),
    };
    let json = serde_json::to_vec(&header).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    let mut out = BufWriter::new(File::create(&tmp).map_err(err)?);
    out.write_all(MAGIC).map_err(err)?;
    out.write_all(&(json.len() as u64).to_le_bytes()).map_err(err)?;
    out.write_all(&json).map_err(err)?;
    for (t, saved) in result.tables.iter().zip(&header.tables) {
        if saved.size > 0 {
            let mut f = File::open(&t.path).map_err(err)?.take(saved.size);
            std::io::copy(&mut f, &mut out).map_err(err)?;
        }
    }
    out.flush().map_err(err)?;
    drop(out);
    std::fs::rename(&tmp, path).map_err(err)
}

/// The comparison, with its rows copied into a new results folder, and the caller's extra data.
pub fn load(path: &Path) -> Result<(DataComparison, serde_json::Value), String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut f = BufReader::new(File::open(path).map_err(err)?);
    let mut magic = vec![0u8; MAGIC.len()];
    f.read_exact(&mut magic).map_err(err)?;
    if magic != MAGIC {
        return Err(format!("{} is not a saved QuillSQL data comparison", path.display()));
    }
    let mut len = [0u8; 8];
    f.read_exact(&mut len).map_err(err)?;
    let mut json = vec![0u8; u64::from_le_bytes(len) as usize];
    f.read_exact(&mut json).map_err(err)?;
    let header: Header = serde_json::from_slice(&json).map_err(|e| format!("{}: {e}", path.display()))?;
    let session = Session::create().map_err(|e| format!("cannot create the results folder: {e}"))?;
    let mut tables = Vec::new();
    for (i, t) in header.tables.into_iter().enumerate() {
        let file = session.table_path(i);
        let mut out = File::create(&file).map_err(err)?;
        std::io::copy(&mut (&mut f).take(t.size), &mut out).map_err(err)?;
        tables.push(TableResult {
            pair: t.pair,
            counts: t.counts,
            offsets: t.offsets,
            path: file,
            error: t.error,
            by_checksum: t.by_checksum,
            repeated_keys: t.repeated_keys,
            identity_current: (t.identity_current.0.map(i128::from), t.identity_current.1.map(i128::from)),
            column_differences: t.column_differences,
        });
    }
    let result = DataComparison { session, tables, only_in_1: header.only_in_1, only_in_2: header.only_in_2, ddl_triggers: header.ddl_triggers, excluded: header.excluded, kinds: header.kinds, masked_reads: false, old_server: false };
    Ok((result, header.extra))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::store::{RowKind, StoredRow, TableWriter, read_all};
    use crate::data::value::Value;

    #[test]
    fn save_and_open() {
        let session = Session::create().unwrap();
        let path = session.table_path(0);
        let mut w = TableWriter::create(&path, true).unwrap();
        let row = StoredRow { kind: RowKind::OnlyIn1, source: vec![Value::Int(7), Value::Str("x".repeat(100))], target: vec![], differs: vec![] };
        w.write(&row).unwrap();
        let pair = TablePair {
            source: ObjectName::new("dbo", "T"),
            target: ObjectName::new("dbo", "T"),
            columns: vec![],
            key: vec![],
            key_name: None,
            target_key_name: None,
            where1: None,
            where2: None,
            problem: None,
            is_view: false,
            notes: vec![],
            excluded: None,
            target_missing: false,
        };
        let t = TableResult {
            pair,
            counts: [0, 0, 1, 0],
            offsets: w.finish().unwrap(),
            path,
            error: None,
            by_checksum: false,
            repeated_keys: false,
            identity_current: (Some(5), None),
            column_differences: vec![],
        };
        let result = DataComparison { session, tables: vec![t], only_in_1: vec![ObjectName::new("dbo", "Old")], only_in_2: vec![], ddl_triggers: vec![], excluded: vec![], kinds: Default::default(), masked_reads: false, old_server: false };
        let file = result.session.dir.join("saved.qdcr");
        save(&result, serde_json::json!({"note": "hi"}), &file).unwrap();
        let (back, extra) = load(&file).unwrap();
        assert_eq!(extra["note"], "hi");
        assert_eq!(back.only_in_1, result.only_in_1);
        assert_eq!(back.tables[0].identity_current, (Some(5), None));
        assert_eq!(read_all(&back.tables[0].path, &back.tables[0].offsets[RowKind::OnlyIn1 as usize]).unwrap(), vec![row]);
        assert!(load(&result.tables[0].path).is_err());
    }
}
