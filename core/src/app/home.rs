extern crate alloc;

use alloc::{string::{String, ToString}, vec::Vec};

use crate::image_viewer::{ImageEntry, ImageData};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartMenuSection {
    Recents,
    Actions,
}

#[derive(Clone, Copy, Debug)]
pub enum StartMenuAction {
    FileBrowser,
    Settings,
    Battery,
}

pub struct RecentPreview {
    pub path: String,
    pub title: String,
    pub image: Option<ImageData>,
}

pub struct HomeState {
    pub entries: Vec<ImageEntry>,
    pub selected: usize,
    pub path: Vec<String>,
    pub start_menu_section: StartMenuSection,
    pub start_menu_index: usize,
    pub start_menu_prev_section: StartMenuSection,
    pub start_menu_prev_index: usize,
    pub start_menu_cache: Vec<RecentPreview>,
    pub start_menu_nav_pending: bool,
    pub start_menu_need_base_refresh: bool,
}

impl HomeState {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            path: Vec::new(),
            start_menu_section: StartMenuSection::Recents,
            start_menu_index: 0,
            start_menu_prev_section: StartMenuSection::Recents,
            start_menu_prev_index: 0,
            start_menu_cache: Vec::new(),
            start_menu_nav_pending: false,
            start_menu_need_base_refresh: true,
        }
    }

    pub fn set_entries(&mut self, entries: Vec<ImageEntry>) {
        self.entries = entries;
        if self.selected >= self.entries.len() {
            self.selected = 0;
        }
    }

    pub fn menu_title(&self) -> String {
        if self.path.is_empty() {
            "Images".to_string()
        } else {
            let mut title = String::from("Images/");
            title.push_str(&self.path.join("/"));
            title
        }
    }

    pub fn entry_path_string(&self, entry: &ImageEntry) -> String {
        let mut parts = self.path.clone();
        parts.push(entry.name.clone());
        parts.join("/")
    }

    pub fn current_entry_name_owned(&self) -> Option<String> {
        let entry = self.entries.get(self.selected)?;
        if entry.kind != crate::image_viewer::EntryKind::File {
            return None;
        }
        Some(self.entry_path_string(entry))
    }
}
