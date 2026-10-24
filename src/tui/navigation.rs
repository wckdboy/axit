use super::*;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

impl App {
    pub(super) fn location(&self) -> Location {
        Location {
            view: self.view,
            folder: self
                .index
                .as_ref()
                .and_then(|i| i.path(self.folder).ok())
                .unwrap_or(self.root.clone()),
            focused: self
                .focused()
                .and_then(|id| self.index.as_ref()?.path(id).ok()),
            query: self.query.clone(),
            sort: self.sort,
            offset: self.table.offset(),
        }
    }

    pub(super) fn restore_location(&mut self, location: Location) {
        self.view = location.view;
        self.query = location.query;
        self.sort = location.sort;
        self.focus = Focus::List;
        let index = self.index.as_ref();
        self.folder = location
            .folder
            .ancestors()
            .find_map(|p| index?.find_path(p))
            .unwrap_or(0);
        let focused = location.focused.and_then(|p| index?.find_path(&p));
        self.refresh(focused, location.offset);
        self.ui.hits.clear();
    }

    fn remember(&mut self) {
        self.pressed = None;
        self.drag_scrollbar = false;
        self.back.push(self.location());
        if self.back.len() > 64 {
            self.back.remove(0);
        }
        self.forward.clear();
        self.cancel_plan();
        self.clear_selection();
        self.ui.hits.clear();
    }

    fn set_view(&mut self, view: View) {
        if self.view != view {
            self.remember();
            self.view = view;
            self.refresh(None, 0);
        }
        self.focus = if view == View::Find {
            Focus::Query
        } else {
            Focus::List
        };
        self.activity_scroll = 0;
    }

    fn folder(&mut self, id: u32, focused: Option<u32>) {
        if self.view == View::Space && self.folder == id {
            self.focus = Focus::List;
            if let Some(row) = focused.and_then(|id| self.ids.iter().position(|value| *value == id))
            {
                self.select_row(row);
            }
            return;
        }
        self.remember();
        self.folder = id;
        self.view = View::Space;
        self.focus = Focus::List;
        self.refresh(focused, 0);
    }

    fn parent(&mut self) {
        let Some(index) = &self.index else {
            return;
        };
        let child = if self.view == View::Find {
            self.focused().unwrap_or(self.folder)
        } else {
            self.folder
        };
        if child == 0 {
            return;
        }
        let parent = index.entry(child).map(|e| e.parent).unwrap_or(0);
        self.folder(parent, Some(child));
    }

    fn move_cursor(&mut self, delta: isize, range: bool) {
        let previous = self.table.selected().unwrap_or(0);
        let row = previous
            .saturating_add_signed(delta)
            .min(self.ids.len().saturating_sub(1));
        if range {
            self.range(self.anchor.unwrap_or(previous), row);
        } else {
            self.anchor = None;
        }
        self.select_row(row);
    }

    fn range(&mut self, anchor: usize, row: usize) {
        self.anchor = Some(anchor);
        self.selected.clear();
        self.selected.extend(
            self.ids
                .iter()
                .copied()
                .skip(anchor.min(row))
                .take(anchor.abs_diff(row) + 1),
        );
    }

    fn toggle(&mut self, row: usize) {
        if let Some(id) = self.ids.get(row)
            && !self.selected.remove(id)
        {
            self.selected.insert(*id);
        }
        self.anchor = Some(row);
        self.select_row(row);
    }

    fn wheel(&mut self, delta: isize) {
        let page = self.ui.page.max(1);
        let offset = self
            .table
            .offset()
            .saturating_add_signed(delta)
            .min(self.ids.len().saturating_sub(page));
        let selected = self.table.selected().unwrap_or(0).clamp(
            offset,
            (offset + page - 1)
                .min(self.ids.len().saturating_sub(1))
                .max(offset),
        );
        self.select_row(selected);
        *self.table.offset_mut() = offset;
    }

    pub(super) fn enabled(&self, cmd: Command) -> bool {
        match cmd {
            Command::Back => !self.back.is_empty(),
            Command::Forward => !self.forward.is_empty(),
            Command::Parent => {
                self.folder != 0 || (self.view == View::Find && self.focused().is_some())
            }
            Command::Open | Command::Reveal | Command::Select => {
                self.view != View::Activity && self.focused().is_some() && !self.searching
            }
            Command::Trash | Command::Move | Command::Clean => self.mutable(),
            Command::Undo => !self.busy && !self.auto_busy && self.scan.is_none(),
            Command::Rescan => self.scan.is_none() && !self.applying && !self.auto_busy,
            Command::Auto => self.view == View::Space && !self.busy && !self.auto_busy,
            _ => true,
        }
    }

    pub(super) fn quit(&mut self) -> bool {
        if self.applying || self.auto_busy {
            self.quit_after_operation = true;
            self.status = "Finishing the file operation before quitting…".into();
            false
        } else {
            true
        }
    }

    fn command(&mut self, cmd: Command) -> bool {
        if !self.enabled(cmd) {
            if matches!(cmd, Command::Trash | Command::Clean | Command::Move) {
                self.status =
                    "Choose an item and wait for the current search/scan to finish".into();
            }
            return false;
        }
        match cmd {
            Command::Back => {
                if let Some(previous) = self.back.pop() {
                    self.forward.push(self.location());
                    self.cancel_plan();
                    self.restore_location(previous);
                }
            }
            Command::Forward => {
                if let Some(next) = self.forward.pop() {
                    self.back.push(self.location());
                    self.cancel_plan();
                    self.restore_location(next);
                }
            }
            Command::Parent => self.parent(),
            Command::Root => self.folder(0, None),
            Command::Open => {
                if let Some(entry) = self.focused().and_then(|id| self.index.as_ref()?.entry(id)) {
                    if entry.is_dir() && !entry.is_link() {
                        self.folder(entry.id, None);
                    } else {
                        self.open_modal(Modal::Inspect);
                    }
                }
            }
            Command::Reveal => {
                if let Some(entry) = self.focused().and_then(|id| self.index.as_ref()?.entry(id)) {
                    self.folder(entry.parent, Some(entry.id));
                }
            }
            Command::Find => self.set_view(View::Find),
            Command::Select => {
                if let Some(row) = self.table.selected() {
                    self.toggle(row);
                }
            }
            Command::Trash => self.preview(Action::Trash),
            Command::Clean => self.preview(Action::Clean),
            Command::Move => {
                let path = self
                    .index
                    .as_ref()
                    .and_then(|i| i.path(self.folder).ok())
                    .unwrap_or(self.root.clone());
                self.open_modal(Modal::Destination(Editor::new(
                    path.to_string_lossy().into(),
                )));
                self.modal_focus = 2; // The path field has focus; Tab reaches the buttons.
            }
            Command::Undo => {
                self.busy = true;
                self.applying = true;
                self.status = "Restoring the last reversible move…".into();
                let (state, tx, index) = (self.state.clone(), self.tx.clone(), self.index.clone());
                thread::spawn(move || {
                    let result = ops::undo(&state)
                        .map(|v| {
                            let restored = v["restored"]
                                .as_str()
                                .and_then(|p| index.as_ref()?.find_path(std::path::Path::new(p)));
                            OperationUpdate {
                                summary: format!(
                                    "Restored {}",
                                    v["restored"].as_str().unwrap_or("file")
                                ),
                                hidden: Vec::new(),
                                restored,
                                blocked: Vec::new(),
                            }
                        })
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(Message::Applied(result));
                });
            }
            Command::Rescan => self.scan(),
            Command::Auto => self.open_modal(Modal::Auto),
            Command::Help => self.open_modal(Modal::Help),
            Command::Quit => return self.quit(),
        }
        false
    }

    fn change_sort(&mut self, sort: Sort) {
        let focused = self.focused();
        let offset = self.table.offset();
        let selected = self.selected.clone();
        self.sort = sort;
        if self.view == View::Find {
            self.restore_focus = focused;
            self.search();
        } else {
            self.refresh(focused, offset);
            self.selected = selected
                .into_iter()
                .filter(|id| self.ids.contains(id))
                .collect();
        }
    }

    fn tab(&mut self, reverse: bool) {
        let mut fields = vec![Focus::List];
        if self.view == View::Find {
            fields.insert(0, Focus::Query);
        }
        if self.ui.inspector.width > 0 {
            fields.push(Focus::Inspect);
        }
        fields.push(Focus::Actions);
        let current = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = fields[(current + if reverse { fields.len() - 1 } else { 1 }) % fields.len()];
    }

    fn cycle_actions(&mut self, delta: isize) {
        if self.ui.commands.is_empty() {
            return;
        }
        for _ in 0..self.ui.commands.len() {
            self.action_index = (self.action_index as isize + delta)
                .rem_euclid(self.ui.commands.len() as isize)
                as usize;
            if self.enabled(self.ui.commands[self.action_index]) {
                break;
            }
        }
    }

    pub(super) fn handle_event(&mut self, event: Event) -> bool {
        let checking = self.busy && !self.applying;
        let chosen = checking.then(|| self.chosen());
        let query = checking.then(|| self.query.text.clone());
        let focus = self.focus;
        let quit = self.dispatch_event(event);
        if checking
            && self.busy
            && !self.applying
            && (chosen.as_ref().is_some_and(|ids| *ids != self.chosen())
                || query.as_ref().is_some_and(|text| *text != self.query.text)
                || (self.focus == Focus::Query && focus != Focus::Query)
                || self.modal.is_some())
        {
            self.cancel_plan();
            self.status = "Preview canceled: selection or view changed · no files changed".into();
        }
        quit
    }

    fn dispatch_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => return self.key(key),
            Event::Mouse(mouse) => return self.mouse(mouse),
            Event::Paste(text) => match &mut self.modal {
                Some(Modal::Destination(input)) => {
                    input.insert(&text);
                    self.modal_focus = 2;
                }
                Some(Modal::Preview(plan, input)) if !plan.entries.is_empty() => {
                    input.insert(&text);
                    self.modal_focus = 2;
                }
                None if self.focus == Focus::Query && self.query.insert(&text) => {
                    self.query_changed()
                }
                _ => {}
            },
            Event::Resize(..) => {
                self.ui.hits.clear();
                self.last_click = None;
                self.pressed = None;
                self.drag_scrollbar = false;
            }
            _ => {}
        }
        false
    }

    fn key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if ctrl && key.code == KeyCode::Char('q') {
            return self.quit();
        }
        if self.modal.is_some() {
            if ctrl && key.code == KeyCode::Char('c') {
                self.modal = None;
                return false;
            }
            self.modal_key(key);
            return false;
        }
        if key.code == KeyCode::Esc && self.busy && !self.applying {
            self.cancel_plan();
            self.status = "Preview canceled · no files changed".into();
            return false;
        }
        if ctrl && key.code == KeyCode::Char('c') {
            return self.quit();
        }
        if ctrl && key.code == KeyCode::Char('f') {
            return self.command(Command::Find);
        }
        if alt {
            match key.code {
                KeyCode::Left => return self.command(Command::Back),
                KeyCode::Right => return self.command(Command::Forward),
                KeyCode::Up => return self.command(Command::Parent),
                KeyCode::Home => return self.command(Command::Root),
                _ => {}
            }
        }
        if ctrl && matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            let tabs = [View::Space, View::Find, View::Clean, View::Activity];
            let current = tabs.iter().position(|v| *v == self.view).unwrap_or(0);
            self.set_view(
                tabs[(current
                    + if shift || key.code == KeyCode::BackTab {
                        3
                    } else {
                        1
                    })
                    % 4],
            );
            return false;
        }
        if key.code == KeyCode::Tab || key.code == KeyCode::BackTab {
            self.tab(shift || key.code == KeyCode::BackTab);
            return false;
        }
        if self.focus == Focus::Query {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Down => self.focus = Focus::List,
                _ => {
                    if self.query.key(key) {
                        self.query_changed();
                    }
                }
            }
            return false;
        }
        if self.focus == Focus::Actions {
            match key.code {
                KeyCode::Left | KeyCode::Up => {
                    self.cycle_actions(-1);
                    return false;
                }
                KeyCode::Right | KeyCode::Down => {
                    self.cycle_actions(1);
                    return false;
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some(cmd) = self.ui.commands.get(self.action_index) {
                        return self.command(*cmd);
                    }
                }
                KeyCode::Esc => {
                    self.focus = Focus::List;
                    return false;
                }
                _ => {}
            }
        }
        if self.view == View::Activity || self.focus == Focus::Inspect {
            let (scroll, max) = if self.view == View::Activity {
                (&mut self.activity_scroll, self.ui.activity_max)
            } else {
                (&mut self.inspect_scroll, self.ui.inspect_max)
            };
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    *scroll = scroll.saturating_add(1).min(max);
                    return false;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    *scroll = scroll.saturating_sub(1);
                    return false;
                }
                KeyCode::PageDown => {
                    *scroll = scroll.saturating_add(self.ui.page.max(1) as u16).min(max);
                    return false;
                }
                KeyCode::PageUp => {
                    *scroll = scroll.saturating_sub(self.ui.page.max(1) as u16);
                    return false;
                }
                KeyCode::Home => {
                    *scroll = 0;
                    return false;
                }
                KeyCode::End => {
                    *scroll = max;
                    return false;
                }
                _ => {}
            }
        }
        if (ctrl || key.modifiers.contains(KeyModifiers::SUPER)) && key.code == KeyCode::Char('a') {
            self.selected = self.ids.iter().copied().collect();
            self.status = format!("{} listed items selected · Esc clears", self.selected.len());
            return false;
        }
        if ctrl || alt || key.modifiers.contains(KeyModifiers::SUPER) {
            return false;
        }
        match key.code {
            KeyCode::Char('1') => self.set_view(View::Space),
            KeyCode::Char('2' | '/') => self.set_view(View::Find),
            KeyCode::Char('3') => self.set_view(View::Clean),
            KeyCode::Char('4') => self.set_view(View::Activity),
            KeyCode::Char('q') => return self.command(Command::Quit),
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.command(Command::Help);
            }
            KeyCode::Char('r') => {
                self.command(Command::Rescan);
            }
            KeyCode::Char('a') => {
                self.command(Command::Auto);
            }
            KeyCode::Char('x') => {
                self.command(Command::Clean);
            }
            KeyCode::Delete | KeyCode::Char('t') => {
                self.command(Command::Trash);
            }
            KeyCode::Char('m') => {
                self.command(Command::Move);
            }
            KeyCode::Char('u') => {
                self.command(Command::Undo);
            }
            KeyCode::Char('p') => {
                self.command(Command::Reveal);
            }
            KeyCode::Char('s') => self.change_sort(match self.sort {
                Sort::Name => Sort::Size,
                Sort::Size => Sort::Modified,
                Sort::Modified => Sort::Name,
            }),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1, shift),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1, shift),
            KeyCode::PageDown => self.move_cursor(self.ui.page.max(1) as isize, shift),
            KeyCode::PageUp => self.move_cursor(-(self.ui.page.max(1) as isize), shift),
            KeyCode::Home => self.move_cursor(-(self.ids.len() as isize), shift),
            KeyCode::End => self.move_cursor(self.ids.len() as isize, shift),
            KeyCode::Char(' ') => {
                self.command(Command::Select);
            }
            KeyCode::Esc => {
                if self.focus != Focus::List {
                    self.focus = Focus::List;
                } else if !self.selected.is_empty() {
                    self.clear_selection();
                } else if self.view == View::Find {
                    self.set_view(View::Space);
                }
            }
            KeyCode::Backspace | KeyCode::Left => {
                self.command(Command::Parent);
            }
            KeyCode::Enter | KeyCode::Right => {
                self.command(Command::Open);
            }
            _ => {}
        }
        false
    }

    fn modal_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.modal = None;
            self.ui.hits.clear();
            self.status = "Canceled · no files changed".into();
            return;
        }
        if let Some(Modal::Menu(selected)) = &mut self.modal {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    *selected = (*selected + 1) % MENU.len()
                }
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                    *selected = (*selected + MENU.len() - 1) % MENU.len()
                }
                KeyCode::Enter => self.confirm_modal(),
                _ => {}
            }
            return;
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            let reverse =
                key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            let fields = match &self.modal {
                Some(Modal::Preview(plan, _)) if !plan.entries.is_empty() => 3,
                Some(Modal::Destination(_)) => 3,
                Some(Modal::Auto) => 2,
                _ => 1,
            };
            self.modal_focus = (self.modal_focus + if reverse { fields - 1 } else { 1 }) % fields;
            return;
        }
        if self.modal_focus < 2 && matches!(key.code, KeyCode::Left | KeyCode::Right) {
            if matches!(&self.modal, Some(Modal::Destination(_) | Modal::Auto))
                || matches!(&self.modal,Some(Modal::Preview(plan,_)) if !plan.entries.is_empty())
            {
                self.modal_focus = 1 - self.modal_focus;
            }
            return;
        }
        if key.code == KeyCode::Enter {
            if self.modal_focus == 0 {
                self.modal = None;
                self.ui.hits.clear();
                return;
            }
            if self.modal_focus == 1 {
                self.confirm_modal();
                return;
            }
            if let Some(Modal::Preview(plan, input)) = &self.modal {
                if input.text == phrase(&plan.action) {
                    self.confirm_modal();
                } else {
                    self.status = format!(
                        "Type {} or choose the confirmation button",
                        phrase(&plan.action)
                    );
                }
                return;
            }
            if matches!(self.modal, Some(Modal::Destination(_))) {
                self.confirm_modal();
                return;
            }
        }
        let scroll = match key.code {
            KeyCode::Up => Some(-1),
            KeyCode::Down => Some(1),
            KeyCode::PageUp => Some(-8),
            KeyCode::PageDown => Some(8),
            KeyCode::Home if self.modal_focus != 2 => {
                self.modal_scroll = 0;
                return;
            }
            KeyCode::End if self.modal_focus != 2 => {
                self.modal_scroll = self.ui.modal_max;
                return;
            }
            _ => None,
        };
        if let Some(delta) = scroll {
            self.modal_scroll = self
                .modal_scroll
                .saturating_add_signed(delta)
                .min(self.ui.modal_max);
            return;
        }
        match &mut self.modal {
            Some(Modal::Preview(plan, input)) if !plan.entries.is_empty() => {
                self.modal_focus = 2;
                input.key(key);
            }
            Some(Modal::Destination(input)) => {
                self.modal_focus = 2;
                input.key(key);
            }
            _ => {}
        }
    }

    fn confirm_modal(&mut self) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        self.ui.hits.clear();
        match modal {
            Modal::Preview(plan, input) => {
                if plan.entries.is_empty() {
                    self.modal = Some(Modal::Preview(plan, input));
                    return;
                }
                let confirmation = phrase(&plan.action).to_string();
                self.apply(*plan, confirmation);
            }
            Modal::Destination(input) => {
                if input.text.trim().is_empty() {
                    self.status = "Choose an existing destination folder".into();
                    self.modal = Some(Modal::Destination(input));
                    return;
                }
                let path = if let Some(rest) = input.text.strip_prefix("~/") {
                    crate::home().unwrap_or(self.root.clone()).join(rest)
                } else {
                    let path = PathBuf::from(&input.text);
                    if path.is_absolute() {
                        path
                    } else {
                        self.index
                            .as_ref()
                            .and_then(|i| i.path(self.folder).ok())
                            .unwrap_or(self.root.clone())
                            .join(path)
                    }
                };
                self.preview(Action::Move {
                    destination: path.to_string_lossy().into(),
                });
            }
            Modal::Auto => {
                let result = (|| -> Result<String> {
                    let mut policy = ops::auto_policy(&self.state)?;
                    policy.enabled = !policy.enabled;
                    if policy.enabled {
                        policy.roots = vec![
                            self.index
                                .as_ref()
                                .ok_or_else(|| anyhow::anyhow!("Scan a project first"))?
                                .path(self.folder)?
                                .to_string_lossy()
                                .into(),
                        ];
                    }
                    ops::save_auto(&self.state, &policy)?;
                    Ok(if policy.enabled {
                        "Automatic cleanup enabled for this folder"
                    } else {
                        "Automatic cleanup disabled"
                    }
                    .into())
                })();
                self.open_modal(Modal::Notice(result.unwrap_or_else(|e| format!("{e:#}"))));
            }
            Modal::Menu(selected) => {
                self.command(MENU[selected.min(MENU.len() - 1)].0);
            }
            _ => {}
        }
    }

    fn scrollbar(&mut self, row: u16) {
        let selected = row
            .saturating_sub(self.ui.list.y)
            .min(self.ui.list.height.saturating_sub(1)) as usize
            * self.ids.len().saturating_sub(1)
            / self.ui.list.height.saturating_sub(1).max(1) as usize;
        self.select_row(selected);
    }

    fn click_hit(&mut self, hit: Hit, column: u16) -> bool {
        if self.modal.is_some() {
            match hit {
                Hit::ModalCancel => {
                    self.modal = None;
                    self.ui.hits.clear();
                    self.status = "Canceled · no files changed".into();
                }
                Hit::ModalConfirm => {
                    self.modal_focus = 1;
                    self.confirm_modal();
                }
                Hit::ModalInput => {
                    self.modal_focus = 2;
                    if let Some(Modal::Preview(_, input) | Modal::Destination(input)) =
                        &mut self.modal
                    {
                        input.click(column.saturating_sub(self.ui.modal_input.x));
                    }
                }
                Hit::Menu(i) => {
                    if let Some(Modal::Menu(selected)) = &mut self.modal {
                        *selected = i;
                    }
                    self.confirm_modal();
                }
                _ => {}
            }
            return false;
        }
        match hit {
            Hit::View(view) => self.set_view(view),
            Hit::Query => {
                self.focus = Focus::Query;
                self.query.click(column.saturating_sub(self.ui.query.x));
            }
            Hit::Crumb(id) => self.folder(id, None),
            Hit::Command(cmd) => return self.command(cmd),
            Hit::Sort(sort) => self.change_sort(sort),
            Hit::Inspect => self.focus = Focus::Inspect,
            Hit::List | Hit::Activity => self.focus = Focus::List,
            _ => {}
        }
        false
    }

    fn mouse(&mut self, mouse: MouseEvent) -> bool {
        let hit = self.ui.hit(mouse.column, mouse.row);
        if mouse.kind == MouseEventKind::Moved {
            self.hover = hit.filter(|h| !matches!(h, Hit::Row(_) | Hit::Check(_) | Hit::List));
            return false;
        }
        if let MouseEventKind::ScrollUp | MouseEventKind::ScrollDown = mouse.kind {
            let delta = if mouse.kind == MouseEventKind::ScrollUp {
                -3
            } else {
                3
            };
            if self.modal.is_some() {
                self.modal_scroll = self
                    .modal_scroll
                    .saturating_add_signed(delta)
                    .min(self.ui.modal_max);
            } else if hit == Some(Hit::Inspect) {
                self.inspect_scroll = self
                    .inspect_scroll
                    .saturating_add_signed(delta)
                    .min(self.ui.inspect_max);
            } else if self.view == View::Activity && hit == Some(Hit::Activity) {
                self.activity_scroll = self
                    .activity_scroll
                    .saturating_add_signed(delta)
                    .min(self.ui.activity_max);
            } else if matches!(
                hit,
                Some(Hit::Row(_) | Hit::Check(_) | Hit::List | Hit::Scrollbar)
            ) {
                self.wheel(delta as isize);
            }
            return false;
        }
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
            self.drag_anchor = None;
            self.drag_scrollbar = false;
            let pressed = self.pressed.take();
            if pressed == hit
                && let Some(hit) = hit
            {
                return self.click_hit(hit, mouse.column);
            }
            return false;
        }
        if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
            if self.drag_scrollbar {
                self.scrollbar(mouse.row);
            } else if let Some(anchor) = self.drag_anchor {
                if let Some(Hit::Row(row) | Hit::Check(row)) = hit {
                    self.range(anchor, row);
                    self.select_row(row);
                } else if mouse.column >= self.ui.list.x && mouse.column < self.ui.list.right() {
                    let row = if mouse.row < self.ui.list.y {
                        self.wheel(-1);
                        Some(self.table.offset())
                    } else if mouse.row >= self.ui.list.bottom() {
                        self.wheel(1);
                        Some(
                            (self.table.offset() + self.ui.page.saturating_sub(1))
                                .min(self.ids.len().saturating_sub(1)),
                        )
                    } else {
                        None
                    };
                    if let Some(row) = row {
                        self.range(anchor, row);
                        self.select_row(row);
                    }
                }
            }
            return false;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && !matches!(hit, Some(Hit::Row(_) | Hit::Check(_)))
        {
            self.pressed = hit;
            self.last_click = None;
            if hit == Some(Hit::Scrollbar) {
                self.drag_scrollbar = true;
                self.scrollbar(mouse.row);
            }
            return false;
        }
        if self.modal.is_some() {
            return false;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Middle) {
            return self.command(Command::Back);
        }
        if matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
        ) && let Some(Hit::Row(row) | Hit::Check(row)) = hit
        {
            self.pressed = None;
            self.focus = Focus::List;
            let previous = self.table.selected().unwrap_or(row);
            let left = mouse.kind == MouseEventKind::Down(MouseButton::Left);
            let modified = left
                && mouse
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
            if left && mouse.modifiers.contains(KeyModifiers::SHIFT) {
                self.range(self.anchor.unwrap_or(previous), row);
            } else if modified || (left && matches!(hit, Some(Hit::Check(_)))) {
                self.toggle(row);
            } else if self
                .ids
                .get(row)
                .is_some_and(|id| !self.selected.contains(id))
            {
                self.selected.clear();
                self.anchor = Some(row);
            }
            self.select_row(row);
            self.drag_anchor = Some(row);
            if !left {
                self.open_modal(Modal::Menu(0));
                return false;
            }
            if !modified
                && !mouse.modifiers.contains(KeyModifiers::SHIFT)
                && matches!(hit, Some(Hit::Row(_)))
                && let Some(id) = self.focused()
            {
                let double = self.last_click.is_some_and(|(old, time)| {
                    old == id && time.elapsed() < Duration::from_millis(400)
                });
                self.last_click = Some((id, Instant::now()));
                if double {
                    self.last_click = None;
                    return self.command(Command::Open);
                }
            }
        }
        false
    }
}

pub(super) const MENU: [(Command, &str); 6] = [
    (Command::Open, "Open / inspect"),
    (Command::Reveal, "Show containing folder"),
    (Command::Select, "Toggle selection"),
    (Command::Trash, "Move to Trash…"),
    (Command::Move, "Move to folder…"),
    (Command::Clean, "Verify cache cleanup…"),
];
pub(super) fn phrase(action: &Action) -> &'static str {
    match action {
        Action::Clean => "CLEAN",
        Action::Trash => "TRASH",
        Action::Move { .. } => "MOVE",
    }
}
