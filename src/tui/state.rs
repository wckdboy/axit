use super::*;
use serde::{Deserialize, Serialize};
use std::io::Write;

#[derive(Serialize, Deserialize)]
struct Overlay {
    stamp: [u64; 5],
    hidden: Vec<u32>,
    dirty: bool,
}
impl App {
    pub(super) fn load_overlay(&mut self) {
        let Some(index) = &self.index else {
            return;
        };
        let path = self.state.join("tui-state.json");
        if !std::fs::metadata(&path).is_ok_and(|m| m.len() <= 2 * 1024 * 1024) {
            return;
        }
        if let Ok(data) = std::fs::read(&path)
            && let Ok(overlay) = serde_json::from_slice::<Overlay>(&data)
            && overlay.stamp == index.stamp()
        {
            self.hidden = overlay
                .hidden
                .into_iter()
                .filter(|id| *id > 0 && *id < index.count)
                .collect();
            self.needs_rescan = overlay.dirty;
        }
    }
    pub(super) fn save_overlay(&mut self) {
        let Some(index) = &self.index else {
            return;
        };
        let result = (|| -> Result<()> {
            let overlay = Overlay {
                stamp: index.stamp(),
                hidden: self.hidden.iter().copied().take(100_000).collect(),
                dirty: self.needs_rescan,
            };
            let mut file = tempfile::NamedTempFile::new_in(&self.state)?;
            file.write_all(&serde_json::to_vec(&overlay)?)?;
            file.as_file().sync_all()?;
            file.persist(self.state.join("tui-state.json"))?;
            Ok(())
        })();
        if let Err(error) = result {
            self.status = format!("{} · View state could not be saved: {error}", self.status);
        }
    }
}
