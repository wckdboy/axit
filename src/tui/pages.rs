use super::*;
#[derive(Default)]
pub(super) struct State {
    pub cursor: Option<String>,
    pub nav: crate::page::Navigation,
    pub largest: Vec<u32>,
    pub landing: Landing,
    pub deferred: Option<Deferred>,
}
pub(super) enum Deferred {
    Inspect(u32),
    Menu(u32),
}
#[derive(Default)]
pub(super) enum Landing {
    #[default]
    First,
    Last,
    Row(usize),
}
impl App {
    pub(super) fn accept_page(
        &mut self,
        data: crate::page::Page,
        focused: Option<u32>,
        offset: usize,
    ) {
        self.ids = data.ids;
        self.pages.cursor = data.cursor;
        self.pages.nav = data.paging;
        self.pages.largest = data.largest;
        let row = focused
            .and_then(|id| self.ids.iter().position(|value| *value == id))
            .unwrap_or_else(|| match self.pages.landing {
                Landing::First => 0,
                Landing::Last => self.ids.len().saturating_sub(1),
                Landing::Row(n) => n.min(self.ids.len().saturating_sub(1)),
            });
        self.select_row(row);
        *self.table.offset_mut() = offset.min(self.ids.len().saturating_sub(1));
        self.pages.landing = Landing::First;
        if let Some(deferred) = self.pages.deferred.take() {
            let id = match deferred {
                Deferred::Inspect(id) | Deferred::Menu(id) => id,
            };
            if !self.listing
                && !self.searching
                && let Some(row) = self.ids.iter().position(|value| *value == id)
            {
                self.select_row(row);
                self.open_modal(match deferred {
                    Deferred::Inspect(_) => Modal::Inspect,
                    Deferred::Menu(_) => Modal::Menu(0),
                });
            }
        }
    }
    pub(super) fn accept_search(&mut self, data: crate::page::Page) {
        self.searching = false;
        self.search_inflight = false;
        self.matched = data.matched;
        self.search_ms = data.elapsed_ms;
        let focused = self.restore_focus.take().or_else(|| self.focused());
        self.accept_page(data, focused, self.table.offset());
        self.status = if self.ids.is_empty() {
            "No matches · edit the query or press Esc to browse".into()
        } else {
            "Ready".into()
        };
    }
    pub(super) fn page_failed(&mut self, error: String) {
        self.ids.clear();
        self.table.select(None);
        self.pages = Default::default();
        self.searching = false;
        self.listing = false;
        self.matched = 0;
        self.status = "Could not load this page · Home restarts · / edits search".into();
        self.search_error = Some(error);
    }
    pub(super) fn change_page(&mut self, cursor: Option<String>, landing: Landing) {
        self.cancel_plan();
        self.invalidate_search();
        self.pages.cursor = cursor;
        self.pages.landing = landing;
        self.pages.deferred = None;
        self.restore_focus = None;
        self.table = TableState::default();
        self.ui.hits.clear();
        self.last_click = None;
        self.pressed = None;
        self.anchor = None;
        if self.view == View::Find {
            self.search();
        } else {
            self.listing(None, 0);
        }
    }
    pub(super) fn page_command(&mut self, cmd: Command) -> bool {
        let (cursor, landing) = match cmd {
            Command::NextPage => (self.pages.nav.next_cursor.clone(), Landing::First),
            Command::PreviousPage => (self.pages.nav.previous_cursor.clone(), Landing::Last),
            Command::FirstPage => (self.pages.nav.first_cursor.clone(), Landing::First),
            Command::LastPage => (self.pages.nav.last_cursor.clone(), Landing::Last),
            _ => return false,
        };
        if cursor.is_none() {
            if matches!(cmd, Command::FirstPage)
                && (self.pages.cursor.is_some() || self.search_error.is_some())
            {
                self.change_page(None, Landing::First);
                return true;
            }
            match cmd {
                Command::FirstPage => {
                    self.select_row(0);
                }
                Command::LastPage => {
                    self.select_row(self.ids.len().saturating_sub(1));
                }
                _ => {}
            }
        } else {
            self.change_page(cursor, landing);
        }
        true
    }
    pub(super) fn focus_entry(&mut self, id: u32) {
        if !self.listing
            && !self.searching
            && let Some(row) = self.ids.iter().position(|value| *value == id)
        {
            self.select_row(row);
            return;
        }
        let Some(index) = &self.index else {
            return;
        };
        let cursor = if self.view == View::Space {
            index.children_cursor_at(self.folder, self.sort, &self.hidden, id)
        } else {
            return;
        };
        self.change_page(Some(cursor), Landing::First);
    }
    pub(super) fn tile_action(&mut self, id: u32, inspect: bool) {
        self.focus_entry(id);
        if self.ids.contains(&id) && !self.listing {
            self.open_modal(if inspect {
                Modal::Inspect
            } else {
                Modal::Menu(0)
            });
        } else {
            self.pages.deferred = Some(if inspect {
                Deferred::Inspect(id)
            } else {
                Deferred::Menu(id)
            });
        }
    }
    pub(super) fn surviving_focus(&self) -> Option<u32> {
        let index = self.index.as_ref()?;
        self.focused()
            .filter(|id| !index.is_hidden(*id, &self.hidden))
            .or_else(|| {
                self.ids
                    .iter()
                    .copied()
                    .find(|id| !index.is_hidden(*id, &self.hidden))
            })
    }
}
