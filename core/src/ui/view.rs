use crate::display::RefreshMode;
use crate::framebuffer::DisplayBuffers;

use super::geom::Rect;

extern crate alloc;

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug)]
pub struct RenderRequest {
    pub rect: Rect,
    pub refresh: RefreshMode,
}

#[derive(Default, Debug)]
pub struct RenderQueue {
    requests: Vec<RenderRequest>,
}

impl RenderQueue {
    pub fn push(&mut self, rect: Rect, refresh: RefreshMode) {
        self.requests.push(RenderRequest { rect, refresh });
    }

    pub fn drain(&mut self) -> impl Iterator<Item = RenderRequest> + '_ {
        self.requests.drain(..)
    }

    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }
}

pub struct UiContext<'a> {
    pub buffers: &'a mut DisplayBuffers,
}

pub trait View {
    fn render(&mut self, ctx: &mut UiContext<'_>, rect: Rect, rq: &mut RenderQueue);
}

pub fn flush_queue(
    display: &mut impl crate::display::Display,
    buffers: &mut DisplayBuffers,
    rq: &mut RenderQueue,
    fallback: RefreshMode,
) {
    let mut mode = None;
    for request in rq.drain() {
        mode = Some(match mode {
            Some(current) => max_refresh(current, request.refresh),
            None => request.refresh,
        });
    }
    display.display(buffers, mode.unwrap_or(fallback));
}

fn max_refresh(a: RefreshMode, b: RefreshMode) -> RefreshMode {
    use RefreshMode::*;
    match (a, b) {
        (Full, _) | (_, Full) => Full,
        (Half, _) | (_, Half) => Half,
        _ => Fast,
    }
}
