extern crate alloc;

use alloc::{format, string::{String, ToString}, vec, vec::Vec};

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::BinaryColor,
    prelude::{DrawTarget, OriginDimensions, Point, Primitive, Size},
    primitives::Rectangle,
    text::Text,
    Drawable,
};

use crate::display::{Display, GrayscaleMode, RefreshMode};
use crate::framebuffer::{DisplayBuffers, Rotation, BUFFER_SIZE, HEIGHT as FB_HEIGHT, WIDTH as FB_WIDTH};
use crate::image_viewer::{AppSource, ImageData, ImageEntry, ImageError};
use crate::ui::{flush_queue, ListItem, ListView, Rect, RenderQueue, UiContext, View};

const START_MENU_MARGIN: i32 = 16;
const START_MENU_RECENT_THUMB: i32 = 74;
const START_MENU_ACTION_GAP: i32 = 12;
const HEADER_Y: i32 = 24;
const LIST_TOP: i32 = 60;
const LINE_HEIGHT: i32 = 24;
const LIST_MARGIN_X: i32 = 16;

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

#[derive(Debug)]
pub enum HomeOpenError {
    Empty,
}

#[derive(Debug)]
pub enum HomeOpen {
    EnterDir,
    OpenFile(ImageEntry),
}

pub enum HomeAction {
    None,
    OpenRecent(String),
    OpenFileBrowser,
    OpenSettings,
}

pub enum MenuAction {
    None,
    OpenSelected,
    Back,
    Dirty,
}

pub struct HomeIcons<'a> {
    pub icon_size: i32,
    pub folder_dark: &'a [u8],
    pub folder_light: &'a [u8],
    pub gear_dark: &'a [u8],
    pub gear_light: &'a [u8],
    pub battery_dark: &'a [u8],
    pub battery_light: &'a [u8],
}

pub type DrawTrbkImageFn = fn(
    &mut DisplayBuffers,
    &ImageData,
    &mut Option<(&mut [u8], &mut [u8], &mut bool)>,
    i32,
    i32,
    i32,
    i32,
);

pub struct HomeRenderContext<'a, S: AppSource> {
    pub display_buffers: &'a mut DisplayBuffers,
    pub gray2_lsb: &'a mut [u8],
    pub gray2_msb: &'a mut [u8],
    pub source: &'a mut S,
    pub full_refresh: bool,
    pub battery_percent: Option<u8>,
    pub icons: HomeIcons<'a>,
    pub draw_trbk_image: DrawTrbkImageFn,
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

    pub fn refresh_entries<S: AppSource>(&mut self, source: &mut S) -> Result<(), ImageError> {
        let entries = source.refresh(&self.path)?;
        self.set_entries(entries);
        Ok(())
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

    pub fn open_selected(&mut self) -> Result<HomeOpen, HomeOpenError> {
        if self.entries.is_empty() {
            return Err(HomeOpenError::Empty);
        }
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return Err(HomeOpenError::Empty);
        };
        match entry.kind {
            crate::image_viewer::EntryKind::Dir => {
                self.path.push(entry.name);
                Ok(HomeOpen::EnterDir)
            }
            crate::image_viewer::EntryKind::File => Ok(HomeOpen::OpenFile(entry)),
        }
    }

    pub fn open_index(&mut self, index: usize) -> Option<HomeOpen> {
        if self.entries.is_empty() {
            return None;
        }
        let index = index.min(self.entries.len().saturating_sub(1));
        let Some(entry) = self.entries.get(index).cloned() else {
            return None;
        };
        if entry.kind != crate::image_viewer::EntryKind::File {
            return None;
        }
        self.selected = index;
        Some(HomeOpen::OpenFile(entry))
    }

    pub fn open_recent_path<S: AppSource>(
        &mut self,
        source: &mut S,
        path: &str,
    ) -> Result<(), ImageError> {
        let mut parts: Vec<String> = path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| part.to_string())
            .collect();
        if parts.is_empty() {
            return Ok(());
        }
        let file = parts.pop().unwrap_or_default();
        self.path = parts;
        self.refresh_entries(source)?;
        let idx = self.entries.iter().position(|entry| entry.name == file);
        if let Some(index) = idx {
            self.selected = index;
            Ok(())
        } else {
            Err(ImageError::Message("Recent entry not found.".into()))
        }
    }

    pub fn start_menu_cache_same(&self, recents: &[String]) -> bool {
        recents.len() == self.start_menu_cache.len()
            && recents
                .iter()
                .zip(self.start_menu_cache.iter())
                .all(|(path, cached)| path == &cached.path)
    }

    pub fn handle_start_menu_input(
        &mut self,
        recents: &[String],
        buttons: &crate::input::ButtonState,
    ) -> HomeAction {
        use crate::input::Buttons;

        if buttons.is_pressed(Buttons::Up) {
            self.start_menu_prev_section = self.start_menu_section;
            self.start_menu_prev_index = self.start_menu_index;
            if self.start_menu_section == StartMenuSection::Recents {
                if self.start_menu_index > 0 {
                    self.start_menu_index -= 1;
                } else {
                    self.start_menu_section = StartMenuSection::Actions;
                    self.start_menu_index = 2;
                }
            } else if self.start_menu_section == StartMenuSection::Actions {
                if self.start_menu_index == 0 && !recents.is_empty() {
                    self.start_menu_section = StartMenuSection::Recents;
                    self.start_menu_index = recents.len().saturating_sub(1);
                } else {
                    self.start_menu_index = self.start_menu_index.saturating_sub(1);
                }
            }
            self.start_menu_nav_pending = true;
            return HomeAction::None;
        }

        if buttons.is_pressed(Buttons::Down) {
            self.start_menu_prev_section = self.start_menu_section;
            self.start_menu_prev_index = self.start_menu_index;
            if self.start_menu_section == StartMenuSection::Recents {
                if self.start_menu_index + 1 < recents.len() {
                    self.start_menu_index += 1;
                } else {
                    self.start_menu_section = StartMenuSection::Actions;
                    self.start_menu_index = 0;
                }
            } else if self.start_menu_section == StartMenuSection::Actions {
                if self.start_menu_index + 1 < 3 {
                    self.start_menu_index += 1;
                }
            }
            self.start_menu_nav_pending = true;
            return HomeAction::None;
        }

        if buttons.is_pressed(Buttons::Left) {
            if self.start_menu_section == StartMenuSection::Actions {
                self.start_menu_prev_section = self.start_menu_section;
                self.start_menu_prev_index = self.start_menu_index;
                self.start_menu_index = self.start_menu_index.saturating_sub(1);
                self.start_menu_nav_pending = true;
            }
            return HomeAction::None;
        }

        if buttons.is_pressed(Buttons::Right) {
            if self.start_menu_section == StartMenuSection::Actions {
                self.start_menu_prev_section = self.start_menu_section;
                self.start_menu_prev_index = self.start_menu_index;
                self.start_menu_index = (self.start_menu_index + 1).min(2);
                self.start_menu_nav_pending = true;
            }
            return HomeAction::None;
        }

        if buttons.is_pressed(Buttons::Confirm) {
            match self.start_menu_section {
                StartMenuSection::Recents => {
                    if let Some(path) = recents.get(self.start_menu_index) {
                        return HomeAction::OpenRecent(path.clone());
                    }
                }
                StartMenuSection::Actions => {
                    return match self.start_menu_index {
                        0 => HomeAction::OpenFileBrowser,
                        1 => HomeAction::OpenSettings,
                        _ => HomeAction::None,
                    };
                }
            }
        }

        HomeAction::None
    }

    pub fn handle_menu_input(
        &mut self,
        buttons: &crate::input::ButtonState,
    ) -> MenuAction {
        use crate::input::Buttons;

        if buttons.is_pressed(Buttons::Up) {
            if !self.entries.is_empty() {
                self.selected = self.selected.saturating_sub(1);
            }
            return MenuAction::Dirty;
        }
        if buttons.is_pressed(Buttons::Down) {
            if !self.entries.is_empty() {
                self.selected = (self.selected + 1).min(self.entries.len() - 1);
            }
            return MenuAction::Dirty;
        }
        if buttons.is_pressed(Buttons::Confirm) {
            return MenuAction::OpenSelected;
        }
        if buttons.is_pressed(Buttons::Back) {
            return MenuAction::Back;
        }

        MenuAction::None
    }

    pub fn draw_start_menu<S: AppSource>(
        &mut self,
        ctx: &mut HomeRenderContext<'_, S>,
        display: &mut impl Display,
        recents: &[String],
    ) {
        let size = ctx.display_buffers.size();
        let width = size.width as i32;
        let height = size.height as i32;
        let mid_y = (height * 82) / 100;

        self.ensure_start_menu_cache(ctx, recents);

        let list_top = HEADER_Y + 24;
        let max_items = 6usize;
        let list_width = width - (START_MENU_MARGIN * 2);
        let item_height = 99;
        let thumb_size = 74;
        let action_top = mid_y + 17;
        let action_width = (width - (START_MENU_MARGIN * 2) - (START_MENU_ACTION_GAP * 2)) / 3;
        let action_height = 110;

        if self.start_menu_need_base_refresh {
            let (gray2_used, draw_count) = self.render_start_menu_contents(
                ctx,
                true,
                width,
                mid_y,
                list_top,
                max_items,
                list_width,
                item_height,
                thumb_size,
                action_top,
                action_width,
                action_height,
            );
            log::info!(
                "Start menu base render: recents={}, cache={}",
                draw_count,
                self.start_menu_cache.len()
            );
            if gray2_used {
                merge_bw_into_gray2(ctx.display_buffers, ctx.gray2_lsb, ctx.gray2_msb);
                let lsb: &[u8; BUFFER_SIZE] = ctx.gray2_lsb.as_ref().try_into().unwrap();
                let msb: &[u8; BUFFER_SIZE] = ctx.gray2_msb.as_ref().try_into().unwrap();
                display.copy_grayscale_buffers(lsb, msb);
                display.display_absolute_grayscale(GrayscaleMode::Fast);
                ctx.display_buffers.copy_active_to_inactive();
            } else {
                let mut rq = RenderQueue::default();
                rq.push(
                    Rect::new(0, 0, width, height),
                    if ctx.full_refresh {
                        RefreshMode::Full
                    } else {
                        RefreshMode::Fast
                    },
                );
                flush_queue(display, ctx.display_buffers, &mut rq, RefreshMode::Full);
            }
            self.start_menu_need_base_refresh = false;
            self.render_start_menu_contents(
                ctx,
                false,
                width,
                mid_y,
                list_top,
                max_items,
                list_width,
                item_height,
                thumb_size,
                action_top,
                action_width,
                action_height,
            );
            let rect_for = |section: StartMenuSection, index: usize| -> Option<Rect> {
                match section {
                    StartMenuSection::Recents => {
                        if index >= max_items {
                            return None;
                        }
                        let y = list_top + (index as i32 * item_height);
                        if y + item_height > mid_y {
                            return None;
                        }
                        Some(Rect::new(
                            START_MENU_MARGIN - 4,
                            y - 4,
                            list_width + 8,
                            item_height - 4,
                        ))
                    }
                    StartMenuSection::Actions => {
                        if index >= 3 {
                            return None;
                        }
                        let x = START_MENU_MARGIN
                            + index as i32 * (action_width + START_MENU_ACTION_GAP);
                        Some(Rect::new(
                            x - 4,
                            action_top - 4,
                            action_width + 8,
                            action_height + 8,
                        ))
                    }
                }
            };
            if let Some(rect) = rect_for(self.start_menu_section, self.start_menu_index) {
                let mut rq = RenderQueue::default();
                rq.push(rect, RefreshMode::Fast);
                flush_queue(display, ctx.display_buffers, &mut rq, RefreshMode::Fast);
            }
            return;
        }

        let (gray2_used, draw_count) = self.render_start_menu_contents(
            ctx,
            false,
            width,
            mid_y,
            list_top,
            max_items,
            list_width,
            item_height,
            thumb_size,
            action_top,
            action_width,
            action_height,
        );
        log::info!(
            "Start menu render: recents={}, cache={}",
            draw_count,
            self.start_menu_cache.len()
        );
        if gray2_used {
            if self.start_menu_nav_pending {
                let mut rq = RenderQueue::default();
                let mut push_rect = |rect: Rect| {
                    rq.push(rect, RefreshMode::Fast);
                };
                let rect_for = |section: StartMenuSection, index: usize| -> Option<Rect> {
                    match section {
                        StartMenuSection::Recents => {
                            if index >= max_items {
                                return None;
                            }
                            let y = list_top + (index as i32 * item_height);
                            if y + item_height > mid_y {
                                return None;
                            }
                            Some(Rect::new(
                                START_MENU_MARGIN - 4,
                                y - 4,
                                list_width + 8,
                                item_height - 4,
                            ))
                        }
                        StartMenuSection::Actions => {
                            if index >= 3 {
                                return None;
                            }
                            let x = START_MENU_MARGIN
                                + index as i32 * (action_width + START_MENU_ACTION_GAP);
                            Some(Rect::new(
                                x - 4,
                                action_top - 4,
                                action_width + 8,
                                action_height + 8,
                            ))
                        }
                    }
                };
                if let Some(rect) =
                    rect_for(self.start_menu_prev_section, self.start_menu_prev_index)
                {
                    push_rect(rect);
                }
                if (self.start_menu_prev_section != self.start_menu_section)
                    || (self.start_menu_prev_index != self.start_menu_index)
                {
                    if let Some(rect) = rect_for(self.start_menu_section, self.start_menu_index) {
                        push_rect(rect);
                    }
                }
                flush_queue(display, ctx.display_buffers, &mut rq, RefreshMode::Fast);
                self.start_menu_nav_pending = false;
            } else {
                let mut rq = RenderQueue::default();
                rq.push(Rect::new(0, 0, width, height), RefreshMode::Fast);
                flush_queue(display, ctx.display_buffers, &mut rq, RefreshMode::Fast);
            }
        } else {
            let mut rq = RenderQueue::default();
            rq.push(
                Rect::new(0, 0, width, height),
                if ctx.full_refresh {
                    RefreshMode::Full
                } else {
                    RefreshMode::Fast
                },
            );
            flush_queue(display, ctx.display_buffers, &mut rq, RefreshMode::Full);
        }
    }

    pub fn draw_menu<S: AppSource>(
        &mut self,
        ctx: &mut HomeRenderContext<'_, S>,
        display: &mut impl Display,
    ) {
        let mut labels: Vec<String> = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            if entry.kind == crate::image_viewer::EntryKind::Dir {
                let mut label = entry.name.clone();
                label.push('/');
                labels.push(label);
            } else {
                labels.push(entry.name.clone());
            }
        }
        let items: Vec<ListItem<'_>> = labels
            .iter()
            .map(|label| ListItem { label: label.as_str() })
            .collect();

        let title = self.menu_title();
        let mut list = ListView::new(&items);
        list.title = Some(title.as_str());
        list.footer = Some("Up/Down: select  Confirm: open  Back: up");
        list.empty_label = Some("No entries found in /images");
        list.selected = self.selected;
        list.margin_x = LIST_MARGIN_X;
        list.header_y = HEADER_Y;
        list.list_top = LIST_TOP;
        list.line_height = LINE_HEIGHT;

        let size = ctx.display_buffers.size();
        let rect = Rect::new(0, 0, size.width as i32, size.height as i32);
        let mut rq = RenderQueue::default();
        let mut ui = UiContext {
            buffers: ctx.display_buffers,
        };
        list.render(&mut ui, rect, &mut rq);

        let fallback = if ctx.full_refresh {
            RefreshMode::Full
        } else {
            RefreshMode::Fast
        };
        flush_queue(display, ctx.display_buffers, &mut rq, fallback);
    }

    fn render_start_menu_contents<S: AppSource>(
        &mut self,
        ctx: &mut HomeRenderContext<'_, S>,
        suppress_selection: bool,
        width: i32,
        mid_y: i32,
        list_top: i32,
        max_items: usize,
        list_width: i32,
        item_height: i32,
        thumb_size: i32,
        action_top: i32,
        action_width: i32,
        action_height: i32,
    ) -> (bool, usize) {
        let header_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        ctx.display_buffers.clear(BinaryColor::On).ok();
        ctx.gray2_lsb.fill(0);
        ctx.gray2_msb.fill(0);
        let mut gray2_used = false;

        Text::new("Recents", Point::new(START_MENU_MARGIN, HEADER_Y), header_style)
            .draw(ctx.display_buffers)
            .ok();

        let mut draw_count = 0usize;
        for (idx, preview) in self.start_menu_cache.iter().take(max_items).enumerate() {
            let y = list_top + (idx as i32 * item_height);
            if y + item_height > mid_y {
                break;
            }
            let is_selected = !suppress_selection
                && self.start_menu_section == StartMenuSection::Recents
                && self.start_menu_index == idx;
            if is_selected {
                Rectangle::new(
                    Point::new(START_MENU_MARGIN - 4, y - 4),
                    Size::new((list_width + 8) as u32, (item_height - 4) as u32),
                )
                .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_fill(
                    BinaryColor::Off,
                ))
                .draw(ctx.display_buffers)
                .ok();
            }
            let thumb_x = START_MENU_MARGIN;
            let thumb_y = y + (item_height - thumb_size) / 2 - 2;
            Rectangle::new(
                Point::new(thumb_x, thumb_y),
                Size::new(thumb_size as u32, thumb_size as u32),
            )
            .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_stroke(
                if is_selected {
                    BinaryColor::On
                } else {
                    BinaryColor::Off
                },
                1,
            ))
            .draw(ctx.display_buffers)
            .ok();
            if let Some(image) = preview.image.as_ref() {
                if let Some(mono) = thumbnail_to_mono(image) {
                    let mut gray2_ctx = None;
                    (ctx.draw_trbk_image)(
                        ctx.display_buffers,
                        &mono,
                        &mut gray2_ctx,
                        thumb_x + 2,
                        thumb_y + 2,
                        thumb_size - 4,
                        thumb_size - 4,
                    );
                } else {
                    let gray2_lsb = &mut *ctx.gray2_lsb;
                    let gray2_msb = &mut *ctx.gray2_msb;
                    let mut gray2_ctx = Some((gray2_lsb, gray2_msb, &mut gray2_used));
                    (ctx.draw_trbk_image)(
                        ctx.display_buffers,
                        &image,
                        &mut gray2_ctx,
                        thumb_x + 2,
                        thumb_y + 2,
                        thumb_size - 4,
                        thumb_size - 4,
                    );
                }
            }
            let text_color = if is_selected {
                BinaryColor::On
            } else {
                BinaryColor::Off
            };
            let label_style = MonoTextStyle::new(&FONT_10X20, text_color);
            Text::new(
                &preview.title,
                Point::new(thumb_x + thumb_size + 12, y + 26),
                label_style,
            )
            .draw(ctx.display_buffers)
            .ok();
            draw_count += 1;
        }
        if draw_count == 0 {
            Text::new(
                "No recent items.",
                Point::new(START_MENU_MARGIN, list_top + 24),
                header_style,
            )
            .draw(ctx.display_buffers)
            .ok();
        }

        Rectangle::new(
            Point::new(START_MENU_MARGIN, mid_y),
            Size::new((width - (START_MENU_MARGIN * 2)) as u32, 1),
        )
        .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_fill(
            BinaryColor::Off,
        ))
        .draw(ctx.display_buffers)
        .ok();

        let actions = [
            (StartMenuAction::FileBrowser, "Files"),
            (StartMenuAction::Settings, "Settings"),
            (StartMenuAction::Battery, ""),
        ];
        for (idx, (_, label)) in actions.iter().enumerate() {
            let x = START_MENU_MARGIN + idx as i32 * (action_width + START_MENU_ACTION_GAP);
            let y = action_top;
            let is_selected = !suppress_selection
                && self.start_menu_section == StartMenuSection::Actions
                && self.start_menu_index == idx;
            if is_selected {
                Rectangle::new(
                    Point::new(x - 4, y - 4),
                    Size::new((action_width + 8) as u32, (action_height + 8) as u32),
                )
                .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_fill(
                    BinaryColor::Off,
                ))
                .draw(ctx.display_buffers)
                .ok();
            }
            Rectangle::new(
                Point::new(x, y),
                Size::new(action_width as u32, action_height as u32),
            )
            .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_stroke(
                if is_selected {
                    BinaryColor::On
                } else {
                    BinaryColor::Off
                },
                1,
            ))
            .draw(ctx.display_buffers)
            .ok();
            let icon_size = ctx.icons.icon_size;
            let icon_x = x + (action_width - icon_size) / 2;
            let icon_y = y + 5;
            match idx {
                0 => draw_icon_gray2(
                    ctx.display_buffers,
                    ctx.gray2_lsb,
                    ctx.gray2_msb,
                    &mut gray2_used,
                    icon_x,
                    icon_y,
                    icon_size,
                    icon_size,
                    ctx.icons.folder_dark,
                    ctx.icons.folder_light,
                ),
                1 => draw_icon_gray2(
                    ctx.display_buffers,
                    ctx.gray2_lsb,
                    ctx.gray2_msb,
                    &mut gray2_used,
                    icon_x,
                    icon_y,
                    icon_size,
                    icon_size,
                    ctx.icons.gear_dark,
                    ctx.icons.gear_light,
                ),
                _ => draw_icon_gray2(
                    ctx.display_buffers,
                    ctx.gray2_lsb,
                    ctx.gray2_msb,
                    &mut gray2_used,
                    icon_x,
                    icon_y,
                    icon_size,
                    icon_size,
                    ctx.icons.battery_dark,
                    ctx.icons.battery_light,
                ),
            }
            let label_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
            Text::new(
                label,
                Point::new(
                    x + (action_width - (label.len() as i32 * 10)) / 2,
                    y + action_height - 12,
                ),
                label_style,
            )
            .draw(ctx.display_buffers)
            .ok();
            if idx == 2 {
                let text = match ctx.battery_percent {
                    Some(value) => format!("{}%", value),
                    None => "--%".to_string(),
                };
                let label_width = (text.len() as i32) * 10;
                let label_x = x + (action_width - label_width) / 2;
                Text::new(
                    &text,
                    Point::new(label_x, y + action_height - 12),
                    label_style,
                )
                .draw(ctx.display_buffers)
                .ok();
            }
        }

        (gray2_used, draw_count)
    }

    fn ensure_start_menu_cache<S: AppSource>(
        &mut self,
        ctx: &mut HomeRenderContext<'_, S>,
        recents: &[String],
    ) {
        if self.start_menu_cache_same(recents) {
            return;
        }
        self.start_menu_cache.clear();
        for path in recents {
            let (title, image) = self.load_recent_preview(ctx, path);
            self.start_menu_cache.push(RecentPreview {
                path: path.clone(),
                title,
                image,
            });
        }
        self.start_menu_need_base_refresh = true;
    }

    fn load_recent_preview<S: AppSource>(
        &mut self,
        ctx: &mut HomeRenderContext<'_, S>,
        path: &str,
    ) -> (String, Option<ImageData>) {
        let label_fallback = basename_from_path(path);
        if let Some(image) = ctx.source.load_thumbnail(path) {
            let title = ctx
                .source
                .load_thumbnail_title(path)
                .filter(|value| !value.is_empty())
                .unwrap_or(label_fallback);
            if let Some(mono) = thumbnail_to_mono(&image) {
                if !matches!(image, ImageData::Mono1 { .. }) {
                    ctx.source.save_thumbnail(path, &mono);
                }
                return (title, Some(mono));
            }
            let needs_resize = match &image {
                ImageData::Mono1 { width, height, .. }
                | ImageData::Gray8 { width, height, .. }
                | ImageData::Gray2 { width, height, .. }
                | ImageData::Gray2Stream { width, height, .. } => {
                    *width != START_MENU_RECENT_THUMB as u32
                        || *height != START_MENU_RECENT_THUMB as u32
                }
            };
            if needs_resize {
                if let Some(thumb) = thumbnail_from_image(&image, START_MENU_RECENT_THUMB as u32) {
                    ctx.source.save_thumbnail(path, &thumb);
                    return (title, Some(thumb));
                }
            }
            return (title, Some(image));
        }
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".tri") || lower.ends_with(".trimg") {
            let mut parts: Vec<String> = path
                .split('/')
                .filter(|part| !part.is_empty())
                .map(|part| part.to_string())
                .collect();
            if parts.is_empty() {
                return (label_fallback, None);
            }
            let file = parts.pop().unwrap_or_default();
            let entry = ImageEntry {
                name: file,
                kind: crate::image_viewer::EntryKind::File,
            };
            if let Ok(image) = ctx.source.load(&parts, &entry) {
                if let ImageData::Gray2Stream { width, height, key } = &image {
                    if let Some(thumb) = ctx.source.load_gray2_stream_thumbnail(
                        key,
                        *width,
                        *height,
                        74,
                        74,
                    ) {
                        ctx.source.save_thumbnail(path, &thumb);
                        return (label_fallback, Some(thumb));
                    }
                }
                if let Some(thumb) = thumbnail_from_image(&image, 74) {
                    ctx.source.save_thumbnail(path, &thumb);
                    return (label_fallback, Some(thumb));
                }
            }
            return (label_fallback, None);
        }
        if !lower.ends_with(".trbk") {
            return (label_fallback, None);
        }
        let mut parts: Vec<String> = path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| part.to_string())
            .collect();
        if parts.is_empty() {
            return (label_fallback, None);
        }
        let file = parts.pop().unwrap_or_default();
        let entry = ImageEntry {
            name: file,
            kind: crate::image_viewer::EntryKind::File,
        };
        let info = match ctx.source.open_trbk(&parts, &entry) {
            Ok(info) => info,
            Err(_) => {
                ctx.source.close_trbk();
                return (label_fallback, None);
            }
        };
        let title = if info.metadata.title.is_empty() {
            label_fallback
        } else {
            info.metadata.title.clone()
        };
        let preview = if !info.images.is_empty() {
            ctx.source.trbk_image(0).ok().and_then(|image| {
                if let ImageData::Gray2Stream { width, height, key } = &image {
                    if let Some(thumb) = ctx.source.load_gray2_stream_thumbnail(
                        key,
                        *width,
                        *height,
                        START_MENU_RECENT_THUMB as u32,
                        START_MENU_RECENT_THUMB as u32,
                    ) {
                        return Some(thumb);
                    }
                }
                thumbnail_from_image(&image, START_MENU_RECENT_THUMB as u32)
            })
        } else {
            None
        };
        ctx.source.close_trbk();
        if let Some(image) = preview.as_ref() {
            ctx.source.save_thumbnail(path, image);
            ctx.source.save_thumbnail_title(path, &title);
        }
        (title, preview)
    }
}

pub fn draw_icon_gray2(
    buffers: &mut DisplayBuffers,
    gray2_lsb: &mut [u8],
    gray2_msb: &mut [u8],
    gray2_used: &mut bool,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    dark_mask: &[u8],
    light_mask: &[u8],
) {
    if width <= 0 || height <= 0 {
        return;
    }
    let width_u = width as usize;
    let height_u = height as usize;
    let expected = (width_u * height_u + 7) / 8;
    if dark_mask.len() != expected || light_mask.len() != expected {
        return;
    }
    for yy in 0..height_u {
        for xx in 0..width_u {
            let idx = yy * width_u + xx;
            let byte = idx / 8;
            let bit = 7 - (idx % 8);
            let dark = (dark_mask[byte] >> bit) & 1 == 1;
            let light = (light_mask[byte] >> bit) & 1 == 1;
            if !dark && !light {
                continue;
            }
            *gray2_used = true;
            let dst_x = x + xx as i32;
            let dst_y = y + yy as i32;
            if dark {
                buffers.set_pixel(dst_x, dst_y, BinaryColor::Off);
            } else {
                buffers.set_pixel(dst_x, dst_y, BinaryColor::On);
            }
            let Some((fx, fy)) = map_display_point(buffers.rotation(), dst_x, dst_y) else {
                continue;
            };
            let dst_idx = fy * FB_WIDTH + fx;
            let dst_byte = dst_idx / 8;
            let dst_bit = 7 - (dst_idx % 8);
            if light {
                gray2_lsb[dst_byte] |= 1 << dst_bit;
            }
            if dark {
                gray2_msb[dst_byte] |= 1 << dst_bit;
            }
        }
    }
}

fn map_display_point(rotation: Rotation, x: i32, y: i32) -> Option<(usize, usize)> {
    if x < 0 || y < 0 {
        return None;
    }
    let (x, y) = match rotation {
        Rotation::Rotate0 => (x as usize, y as usize),
        Rotation::Rotate90 => (y as usize, FB_HEIGHT - 1 - x as usize),
        Rotation::Rotate180 => (FB_WIDTH - 1 - x as usize, FB_HEIGHT - 1 - y as usize),
        Rotation::Rotate270 => (FB_WIDTH - 1 - y as usize, x as usize),
    };
    if x >= FB_WIDTH || y >= FB_HEIGHT {
        None
    } else {
        Some((x, y))
    }
}

fn basename_from_path(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn thumbnail_from_image(image: &ImageData, size: u32) -> Option<ImageData> {
    let (src_w, src_h) = match image {
        ImageData::Mono1 { width, height, .. } => (*width, *height),
        ImageData::Gray8 { width, height, .. } => (*width, *height),
        ImageData::Gray2 { width, height, .. } => (*width, *height),
        ImageData::Gray2Stream { width, height, .. } => (*width, *height),
    };
    if src_w == 0 || src_h == 0 {
        return None;
    }
    let dst_w = size;
    let dst_h = size;
    let dst_len = ((dst_w as usize * dst_h as usize) + 7) / 8;
    let mut bits = vec![0xFF; dst_len];
    for y in 0..dst_h {
        for x in 0..dst_w {
            let sx = (x * src_w) / dst_w;
            let sy = (y * src_h) / dst_h;
            let lum = match image {
                ImageData::Mono1 { width, bits, .. } => {
                    let idx = (sy * (*width) + sx) as usize;
                    let byte = bits[idx / 8];
                    let bit = 7 - (idx % 8);
                    if (byte >> bit) & 1 == 1 { 255 } else { 0 }
                }
                ImageData::Gray8 { width, pixels, .. } => {
                    let idx = (sy * (*width) + sx) as usize;
                    pixels.get(idx).copied().unwrap_or(255)
                }
                ImageData::Gray2 { width, height, data, .. } => {
                    let idx = (sy * (*width) + sx) as usize;
                    let byte = idx / 8;
                    let bit = 7 - (idx % 8);
                    let plane_len = (((*width) as usize * (*height) as usize) + 7) / 8;
                    if data.len() < plane_len * 3 {
                        255
                    } else {
                        let bw = (data[byte] >> bit) & 1;
                        let l = (data[plane_len + byte] >> bit) & 1;
                        let m = (data[plane_len * 2 + byte] >> bit) & 1;
                        match (m, l, bw) {
                            (0, 0, 1) => 255,
                            (0, 1, 1) => 192,
                            (1, 0, 0) => 128,
                            (1, 1, 0) => 64,
                            _ => 0,
                        }
                    }
                }
                ImageData::Gray2Stream { .. } => 255,
            };
            let dst_idx = (y * dst_w + x) as usize;
            let dst_byte = dst_idx / 8;
            let dst_bit = 7 - (dst_idx % 8);
            let lum = adjust_thumbnail_luma(lum);
            if lum >= 128 {
                bits[dst_byte] |= 1 << dst_bit;
            } else {
                bits[dst_byte] &= !(1 << dst_bit);
            }
        }
    }
    Some(ImageData::Mono1 {
        width: dst_w,
        height: dst_h,
        bits,
    })
}

fn thumbnail_to_mono(image: &ImageData) -> Option<ImageData> {
    match image {
        ImageData::Mono1 { .. } => Some(image.clone()),
        ImageData::Gray8 { width, height, pixels } => {
            let plane = ((*width as usize * *height as usize) + 7) / 8;
            let mut bits = vec![0xFF; plane];
            for idx in 0..(*width as usize * *height as usize) {
                let byte = idx / 8;
                let bit = 7 - (idx % 8);
                let lum = pixels.get(idx).copied().unwrap_or(255);
                let lum = adjust_thumbnail_luma(lum);
                if lum >= 128 {
                    bits[byte] |= 1 << bit;
                } else {
                    bits[byte] &= !(1 << bit);
                }
            }
            Some(ImageData::Mono1 {
                width: *width,
                height: *height,
                bits,
            })
        }
        ImageData::Gray2 { width, height, data } => {
            let plane = ((*width as usize * *height as usize) + 7) / 8;
            if data.len() < plane * 3 {
                return None;
            }
            let mut bits = vec![0xFF; plane];
            for idx in 0..(*width as usize * *height as usize) {
                let byte = idx / 8;
                let bit = 7 - (idx % 8);
                let bw = (data[byte] >> bit) & 1;
                let l = (data[plane + byte] >> bit) & 1;
                let m = (data[plane * 2 + byte] >> bit) & 1;
                let lum = match (m, l, bw) {
                    (0, 0, 1) => 255,
                    (0, 1, 1) => 192,
                    (1, 0, 0) => 128,
                    (1, 1, 0) => 64,
                    _ => 0,
                };
                let lum = adjust_thumbnail_luma(lum);
                if lum >= 128 {
                    bits[byte] |= 1 << bit;
                } else {
                    bits[byte] &= !(1 << bit);
                }
            }
            Some(ImageData::Mono1 {
                width: *width,
                height: *height,
                bits,
            })
        }
        ImageData::Gray2Stream { .. } => None,
    }
}

fn adjust_thumbnail_luma(lum: u8) -> u8 {
    let mut value = ((lum as i32 - 128) * 13) / 10 + 128;
    if value < 0 {
        value = 0;
    } else if value > 255 {
        value = 255;
    }
    value as u8
}

pub fn merge_bw_into_gray2(
    display_buffers: &mut DisplayBuffers,
    gray2_lsb: &mut [u8],
    gray2_msb: &mut [u8],
) {
    let size = display_buffers.size();
    let width = size.width as i32;
    let height = size.height as i32;
    for y in 0..height {
        for x in 0..width {
            if read_pixel(display_buffers, x, y) {
                continue;
            }
            let Some((fx, fy)) = map_display_point(display_buffers.rotation(), x, y) else {
                continue;
            };
            let idx = fy * FB_WIDTH + fx;
            let byte = idx / 8;
            let bit = 7 - (idx % 8);
            gray2_lsb[byte] |= 1 << bit;
            gray2_msb[byte] |= 1 << bit;
        }
    }
}

fn read_pixel(display_buffers: &DisplayBuffers, x: i32, y: i32) -> bool {
    let size = display_buffers.size();
    if x < 0 || y < 0 || x as u32 >= size.width || y as u32 >= size.height {
        return true;
    }
    let (x, y) = match display_buffers.rotation() {
        Rotation::Rotate0 => (x as usize, y as usize),
        Rotation::Rotate90 => (y as usize, FB_HEIGHT - 1 - x as usize),
        Rotation::Rotate180 => (FB_WIDTH - 1 - x as usize, FB_HEIGHT - 1 - y as usize),
        Rotation::Rotate270 => (FB_WIDTH - 1 - y as usize, x as usize),
    };
    if x >= FB_WIDTH || y >= FB_HEIGHT {
        return true;
    }
    let index = y * FB_WIDTH + x;
    let byte_index = index / 8;
    let bit_index = 7 - (index % 8);
    let buffer = display_buffers.get_active_buffer();
    (buffer[byte_index] >> bit_index) & 0x01 == 1
}
