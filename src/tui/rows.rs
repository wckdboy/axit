use super::*;
use std::collections::VecDeque;
const CACHE_CAPACITY: usize = 32;
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Key {
    stamp: [u64; 5],
    view: View,
    folder: u32,
    sort: Sort,
    revision: u64,
    query: String,
    cursor: Option<String>,
    locate: Option<u32>,
}
pub(super) use crate::page::Page as Data;
#[derive(Default)]
pub(super) struct Cache(VecDeque<(Key, Data)>);
impl Cache {
    fn get(&mut self, key: &Key) -> Option<Data> {
        let pos = self.0.iter().position(|(k, _)| k == key)?;
        let entry = self.0.remove(pos)?;
        let data = entry.1.clone();
        self.0.push_front(entry);
        Some(data)
    }
    fn insert(&mut self, key: Key, data: Data) {
        self.0.retain(|(k, _)| *k != key);
        self.0.push_front((key, data));
        self.0.truncate(CACHE_CAPACITY);
    }
}
fn clean_spec(sort: Sort, hidden: &HashSet<u32>) -> crate::page::Spec {
    crate::page::Spec {
        sort,
        binding: crate::page::binding(b"clean-candidates", sort, hidden),
        summarize: false,
    }
}
#[derive(Clone, Copy)]
pub(super) struct Listing<'a> {
    pub view: View,
    pub folder: u32,
    pub sort: Sort,
    pub request: crate::page::Request<'a>,
    pub locate: Option<u32>,
}
pub(super) fn compute_page(
    index: &Index,
    listing: Listing<'_>,
    hidden: &HashSet<u32>,
    generation: &AtomicU64,
    ticket: u64,
) -> Result<Option<Data>> {
    let Listing {
        view,
        folder,
        sort,
        request,
        locate,
    } = listing;
    let load = |request| -> Result<Data> {
        match view {
            View::Space => {
                index.children_page(folder, sort, request, hidden, Some((generation, ticket)))
            }
            View::Clean => {
                let candidates = ops::cache_candidates(index);
                crate::page::select(
                    index,
                    clean_spec(sort, hidden),
                    request,
                    candidates.iter().copied(),
                    |e, _| {
                        !index.is_hidden(e.id, hidden) && candidates.binary_search(&e.id).is_ok()
                    },
                    Some((generation, ticket)),
                )
            }
            _ => Ok(Data::default()),
        }
    };
    let mut result = load(request)?;
    if !result.canceled
        && request.cursor.is_none()
        && let Some(id) = locate
        && !result.ids.contains(&id)
    {
        let cursor = if view == View::Space {
            index.children_cursor_at(folder, sort, hidden, id)
        } else {
            crate::page::at(index, clean_spec(sort, hidden), id)
        };
        if let Ok(found) = load(crate::page::Request {
            limit: request.limit,
            cursor: Some(&cursor),
        }) {
            result = found;
        }
    }
    Ok((!result.canceled).then_some(result))
}
impl App {
    fn rows_key(&self, index: &Index, locate: Option<u32>) -> Key {
        Key {
            stamp: index.stamp(),
            view: self.view,
            folder: self.folder,
            sort: self.sort,
            revision: self.rows_revision,
            query: if self.view == View::Find {
                self.query.text.clone()
            } else {
                String::new()
            },
            cursor: self.pages.cursor.clone(),
            locate,
        }
    }
    pub(super) fn cached_page(&self, index: &Index, locate: Option<u32>) -> Option<Data> {
        // A fresh age-filtered search gets a fresh clock. Continuation tokens
        // freeze their clock, so their immutable windows remain cacheable.
        if self.view == View::Find
            && self.pages.cursor.is_none()
            && query::words(&self.query.text)
                .is_ok_and(|words| words.iter().any(|word| word.starts_with("older:")))
        {
            return None;
        }
        let key = self.rows_key(index, locate);
        self.rows_cache
            .lock()
            .ok()
            .and_then(|mut cache| cache.get(&key))
    }
    pub(super) fn cache_page(&self, index: &Index, locate: Option<u32>, data: Data) {
        if let Ok(mut cache) = self.rows_cache.lock() {
            cache.insert(self.rows_key(index, locate), data);
        }
    }
    pub(super) fn invalidate_rows(&mut self) {
        self.rows_revision = self.rows_revision.wrapping_add(1);
        self.pages = Default::default();
        // Continuations also bind the removed-row overlay, not just the index.
        for location in self.back.iter_mut().chain(self.forward.iter_mut()) {
            location.page_cursor = None;
        }
        if let Ok(mut cache) = self.rows_cache.lock() {
            cache.0.clear();
        }
    }
    pub(super) fn apply_listing(&mut self, data: Data, focused: Option<u32>, offset: usize) {
        self.search_error = None;
        self.folder_count = data.matched;
        self.folder_bytes = data.total_bytes;
        self.listing = false;
        self.accept_page(data, focused, offset);
    }
    pub(super) fn listing(&mut self, focused: Option<u32>, offset: usize) {
        let Some(index) = self.index.clone() else {
            return;
        };
        if !matches!(self.view, View::Space | View::Clean) {
            return;
        }
        if let Some(data) = self.cached_page(&index, focused) {
            self.apply_listing(data, focused, offset);
            return;
        }
        if index.count <= 4096 {
            let result = compute_page(
                &index,
                Listing {
                    view: self.view,
                    folder: self.folder,
                    sort: self.sort,
                    request: crate::page::Request {
                        limit: 1000,
                        cursor: self.pages.cursor.as_deref(),
                    },
                    locate: focused,
                },
                &self.hidden,
                &self.generation,
                self.generation.load(Ordering::Relaxed),
            );
            self.receive_listing(result.map_err(|e| e.to_string()), focused, offset);
            return;
        }
        self.listing = true;
        self.pending_listing = Some((focused, offset));
        self.start_listing();
    }
    pub(super) fn receive_listing(
        &mut self,
        result: std::result::Result<Option<Data>, String>,
        focused: Option<u32>,
        offset: usize,
    ) {
        self.listing = false;
        match result {
            Ok(Some(data)) => {
                if let Some(index) = &self.index {
                    self.cache_page(index, focused, data.clone());
                }
                self.apply_listing(data, focused, offset);
            }
            Ok(None) => {}
            Err(error) => {
                self.page_failed(error);
            }
        }
    }
    pub(super) fn start_listing(&mut self) {
        if self.list_inflight || self.pending_listing.is_none() {
            return;
        }
        let Some(index) = self.index.clone() else {
            return;
        };
        let (focused, offset) = self.pending_listing.take().unwrap();
        if let Some(data) = self.cached_page(&index, focused) {
            self.apply_listing(data, focused, offset);
            return;
        }
        self.list_inflight = true;
        let (view, folder, sort, cursor, hidden, generation, tx) = (
            self.view,
            self.folder,
            self.sort,
            self.pages.cursor.clone(),
            self.hidden.clone(),
            self.generation.clone(),
            self.tx.clone(),
        );
        let ticket = generation.load(Ordering::Relaxed);
        thread::spawn(move || {
            let result = compute_page(
                &index,
                Listing {
                    view,
                    folder,
                    sort,
                    request: crate::page::Request {
                        limit: 1000,
                        cursor: cursor.as_deref(),
                    },
                    locate: focused,
                },
                &hidden,
                &generation,
                ticket,
            )
            .map_err(|e| e.to_string());
            let _ = tx.send(Message::Listing(ticket, result, focused, offset));
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn row_cache_is_bounded_and_recent_visits_survive_eviction() {
        let key = |folder| Key {
            stamp: [1; 5],
            view: View::Space,
            folder,
            sort: Sort::Size,
            revision: 0,
            query: String::new(),
            cursor: None,
            locate: None,
        };
        let mut cache = Cache::default();
        for folder in 0..CACHE_CAPACITY as u32 {
            cache.insert(key(folder), Data::default());
        }
        assert!(cache.get(&key(0)).is_some());
        cache.insert(key(100), Data::default());
        assert_eq!(cache.0.len(), CACHE_CAPACITY);
        assert!(cache.get(&key(0)).is_some());
        assert!(cache.get(&key(1)).is_none());
    }
}
