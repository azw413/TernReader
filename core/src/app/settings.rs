extern crate alloc;

use alloc::format;

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::BinaryColor,
    prelude::{DrawTarget, OriginDimensions, Point},
    text::Text,
    Drawable,
};

use crate::{
    app::home::{draw_icon_gray2, merge_bw_into_gray2},
    display::{Display, GrayscaleMode, RefreshMode},
    framebuffer::{DisplayBuffers, BUFFER_SIZE},
    ui::{flush_queue, Rect, RenderQueue},
};

const LIST_MARGIN_X: i32 = 16;
const HEADER_Y: i32 = 24;

pub struct SettingsContext<'a> {
    pub display_buffers: &'a mut DisplayBuffers,
    pub gray2_lsb: &'a mut [u8],
    pub gray2_msb: &'a mut [u8],
    pub logo_w: i32,
    pub logo_h: i32,
    pub logo_dark: &'a [u8],
    pub logo_light: &'a [u8],
    pub version: &'a str,
    pub build_time: &'a str,
}

pub fn draw_settings(ctx: &mut SettingsContext<'_>, display: &mut impl Display) {
    ctx.display_buffers.clear(BinaryColor::On).ok();

    let heading_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
    let body_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);

    let heading = "TernReader Firmware";
    let heading_pos = Point::new(LIST_MARGIN_X, HEADER_Y + 10);
    Text::new(heading, heading_pos, heading_style)
        .draw(ctx.display_buffers)
        .ok();
    Text::new(heading, Point::new(heading_pos.x + 1, heading_pos.y), heading_style)
        .draw(ctx.display_buffers)
        .ok();

    let size = ctx.display_buffers.size();
    let logo_x = ((size.width as i32) - ctx.logo_w) / 2;
    let logo_y = heading_pos.y + 24;
    let mut gray2_used = false;
    draw_icon_gray2(
        ctx.display_buffers,
        ctx.gray2_lsb,
        ctx.gray2_msb,
        &mut gray2_used,
        logo_x,
        logo_y,
        ctx.logo_w,
        ctx.logo_h,
        ctx.logo_dark,
        ctx.logo_light,
    );

    let version_line = format!("Version: {}", ctx.version);
    let time_line = format!("Build time: {}", ctx.build_time);

    let details_y = logo_y + ctx.logo_h + 12;
    Text::new(&version_line, Point::new(LIST_MARGIN_X, details_y), body_style)
        .draw(ctx.display_buffers)
        .ok();
    Text::new(&time_line, Point::new(LIST_MARGIN_X, details_y + 24), body_style)
        .draw(ctx.display_buffers)
        .ok();

    Text::new(
        "Press Back to return",
        Point::new(LIST_MARGIN_X, details_y + 52),
        body_style,
    )
    .draw(ctx.display_buffers)
    .ok();

    if gray2_used {
        merge_bw_into_gray2(ctx.display_buffers, ctx.gray2_lsb, ctx.gray2_msb);
        let lsb_buf: &[u8; BUFFER_SIZE] = (&*ctx.gray2_lsb).try_into().unwrap();
        let msb_buf: &[u8; BUFFER_SIZE] = (&*ctx.gray2_msb).try_into().unwrap();
        display.copy_grayscale_buffers(lsb_buf, msb_buf);
        display.display_absolute_grayscale(GrayscaleMode::Fast);
        ctx.display_buffers.copy_active_to_inactive();
    } else {
        let mut rq = RenderQueue::default();
        rq.push(
            Rect::new(0, 0, size.width as i32, size.height as i32),
            RefreshMode::Full,
        );
        flush_queue(display, ctx.display_buffers, &mut rq, RefreshMode::Full);
    }
}
