use std::sync::{Arc, Mutex};
use usage_core::{paths, Index};

/// Read-only connections kept open between commands.
const KEEP_READERS: usize = 4;

/// The index, opened on first use. One writer connection does refreshes;
/// every command runs on a read-only connection from a small pool, so
/// queries are not blocked while a refresh writes. A refresh called on a
/// reader hands itself to the writer (see `Index::with_writer`), so two
/// refreshes still run one at a time.
#[derive(Clone, Default)]
pub struct IndexSlot {
    writer: Arc<Mutex<Option<Arc<Mutex<Index>>>>>,
    readers: Arc<Mutex<Vec<Index>>>,
}

#[derive(Default)]
pub struct AppState {
    pub index: IndexSlot,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl IndexSlot {
    /// The writer, opening (and migrating) the index the first time.
    fn writer(&self) -> Result<Arc<Mutex<Index>>, String> {
        let mut g = self.writer.lock().map_err(err)?;
        if let Some(w) = g.as_ref() {
            return Ok(w.clone());
        }
        let w = Arc::new(Mutex::new(Index::open(&paths::index_path()).map_err(err)?));
        *g = Some(w.clone());
        Ok(w)
    }

    fn take_reader(&self) -> Result<Index, String> {
        if let Some(r) = self.readers.lock().map_err(err)?.pop() {
            return Ok(r);
        }
        let w = self.writer()?;
        Ok(Index::open_reader(&paths::index_path())
            .map_err(err)?
            .attach_writer(w))
    }

    fn put_reader(&self, r: Index) {
        if let Ok(mut g) = self.readers.lock() {
            if g.len() < KEEP_READERS {
                g.push(r);
            }
        }
    }
}

/// Run `f` on a read-only connection. Writes it makes through `Index`
/// methods (`refresh`, `insert_rows`, `with_writer`) go to the writer.
pub fn with_index<T>(
    slot: &IndexSlot,
    f: impl FnOnce(&mut Index) -> usage_core::Result<T>,
) -> Result<T, String> {
    let mut r = slot.take_reader()?;
    let out = f(&mut r).map_err(err);
    slot.put_reader(r);
    out
}

/// Run `f` on the writer connection directly. Holds the write lock for the
/// whole call; use it only for writes.
#[allow(dead_code)]
pub fn with_writer<T>(
    slot: &IndexSlot,
    f: impl FnOnce(&mut Index) -> usage_core::Result<T>,
) -> Result<T, String> {
    let w = slot.writer()?;
    let mut g = w.lock().map_err(err)?;
    f(&mut g).map_err(err)
}
