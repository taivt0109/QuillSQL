//! The database pages of a backup set: each data stream is a run of whole 64 KB extents, the
//! first page of each naming the extent. A differential backup's extents replace the full
//! backup's; the stripes of a striped set each hold some of the extents.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::mtf::{Medium, SetPart, read_full, u16_at, u32_at};

pub const PAGE: usize = 8192;
const EXTENT: u64 = 65536;

pub type Page = Arc<Vec<u8>>;

/// A page's address.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PageId {
    pub file: u16,
    pub page: u32,
}

impl PageId {
    /// From the 6-byte form stored in pages and catalog rows (page, then file).
    pub fn at(b: &[u8], o: usize) -> PageId {
        PageId { page: u32_at(b, o), file: u16_at(b, o + 4) }
    }

    pub fn is_null(&self) -> bool {
        self.file == 0 && self.page == 0
    }
}

impl std::fmt::Display for PageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.file, self.page)
    }
}

/// What a page header says about the page.
#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub kind: u8,
    pub level: u8,
    pub flags: u16,
    pub prev: PageId,
    pub next: PageId,
    pub slots: u16,
    /// The allocation unit the page belongs to.
    pub alloc_unit: u64,
    pub id: PageId,
}

pub fn header(p: &[u8]) -> Header {
    let index = u16_at(p, 6) as u64;
    let object = u32_at(p, 24) as u64;
    Header {
        kind: p[1],
        level: p[3],
        flags: u16_at(p, 4),
        prev: PageId::at(p, 8),
        next: PageId::at(p, 16),
        slots: u16_at(p, 22),
        alloc_unit: (index << 48) | (object << 16),
        id: PageId::at(p, 32),
    }
}

/// The record at a slot, from the slot array at the end of the page.
pub fn slot(p: &[u8], i: u16) -> Option<&[u8]> {
    let at = PAGE - 2 - 2 * i as usize;
    let off = u16_at(p, at) as usize;
    if !(96..PAGE).contains(&off) {
        return None;
    }
    Some(&p[off..])
}

pub struct PageStore {
    media: Vec<Arc<dyn Medium>>,
    extents: HashMap<(u16, u32), (usize, u64)>,
    cache: Mutex<HashMap<PageId, Page>>,
}

impl PageStore {
    /// The extents of `parts` in order: a later part's copy of an extent replaces an earlier one.
    pub fn build(parts: &[(Arc<dyn Medium>, &SetPart)]) -> PageStore {
        let mut media = Vec::new();
        let mut extents = HashMap::new();
        let mut head = [0u8; 64];
        for (m, part) in parts {
            media.push(m.clone());
            let mi = media.len() - 1;
            for &(off, len) in &part.extents {
                let mut at = off;
                while at + EXTENT <= off + len {
                    if read_full(&**m, at, &mut head) == 64 {
                        let h = header(&head);
                        if h.id.file != 0 && h.kind < 100 {
                            extents.insert((h.id.file, h.id.page & !7), (mi, at - (h.id.page & 7) as u64 * PAGE as u64));
                        }
                    }
                    at += EXTENT;
                }
            }
        }
        PageStore { media, extents, cache: Mutex::new(HashMap::new()) }
    }

    /// A store with no pages (for decoding values that need none).
    pub fn empty() -> PageStore {
        PageStore { media: Vec::new(), extents: HashMap::new(), cache: Mutex::new(HashMap::new()) }
    }

    pub fn page(&self, id: PageId) -> Option<Page> {
        if let Some(p) = self.cache.lock().unwrap().get(&id) {
            return Some(p.clone());
        }
        let &(mi, base) = self.extents.get(&(id.file, id.page & !7))?;
        let mut p = vec![0u8; PAGE];
        if read_full(&*self.media[mi], base + (id.page & 7) as u64 * PAGE as u64, &mut p) != PAGE {
            return None;
        }
        restore_torn_bits(&mut p);
        if header(&p).id != id {
            return None;
        }
        let p = Arc::new(p);
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= 4096 {
            cache.clear();
        }
        cache.insert(id, p.clone());
        Some(p)
    }

    /// Whether the backup holds the extent of `id`.
    pub fn has(&self, id: PageId) -> bool {
        self.extents.contains_key(&(id.file, id.page & !7))
    }
}

/// With torn-page detection the last two bits of each 512-byte sector were replaced by a
/// pattern and saved in the header; puts them back.
fn restore_torn_bits(p: &mut [u8]) {
    if u16_at(p, 4) & 0x100 == 0 {
        return;
    }
    let mut bits = u32_at(p, 60);
    for sector in (2..=16).rev() {
        let at = 512 * sector - 1;
        p[at] = (p[at] & 0xfc) | (bits >> 30) as u8;
        bits <<= 2;
    }
}
