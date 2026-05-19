use anyhow::{Context, Result, bail};
use image::RgbaImage;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ImageOrder, Visualid, Visualtype};

use crate::x11::X11Context;

#[derive(Debug, Clone, Copy)]
struct PixelFormat {
    bits_per_pixel: u8,
    bytes_per_pixel: usize,
    scanline_pad: u8,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
    lsb_first: bool,
}

pub fn ximage_to_rgba(
    ctx: &X11Context,
    visual: Visualid,
    depth: u8,
    width: u16,
    height: u16,
    data: &[u8],
) -> Result<RgbaImage> {
    let format = pixel_format(ctx, visual, depth)?;
    let stride = stride(width, format.bits_per_pixel, format.scanline_pad)?;
    let required = stride
        .checked_mul(height as usize)
        .context("XImage stride overflow")?;
    if data.len() < required {
        bail!(
            "XImage data is too short: got {} bytes, need at least {}",
            data.len(),
            required
        );
    }

    let mut out = RgbaImage::new(width as u32, height as u32);
    for y in 0..height as usize {
        let row = y * stride;
        for x in 0..width as usize {
            let offset = row + x * format.bytes_per_pixel;
            let pixel = read_pixel(
                &data[offset..offset + format.bytes_per_pixel],
                format.bytes_per_pixel,
                format.lsb_first,
            );
            let red = extract_channel(pixel, format.red_mask);
            let green = extract_channel(pixel, format.green_mask);
            let blue = extract_channel(pixel, format.blue_mask);
            out.put_pixel(x as u32, y as u32, image::Rgba([red, green, blue, 255]));
        }
    }
    Ok(out)
}

pub fn rgba_to_zpixmap(ctx: &X11Context, image: &RgbaImage) -> Result<Vec<u8>> {
    let format = pixel_format(ctx, ctx.root_visual, ctx.root_depth)?;
    let width = u16::try_from(image.width()).context("image too wide for X11 put_image")?;
    let height = u16::try_from(image.height()).context("image too tall for X11 put_image")?;
    let stride = stride(width, format.bits_per_pixel, format.scanline_pad)?;
    let mut out = vec![0u8; stride * height as usize];

    for y in 0..height as usize {
        let row = y * stride;
        for x in 0..width as usize {
            let rgba = image.get_pixel(x as u32, y as u32).0;
            let pixel = encode_channel(rgba[0], format.red_mask)
                | encode_channel(rgba[1], format.green_mask)
                | encode_channel(rgba[2], format.blue_mask);
            let offset = row + x * format.bytes_per_pixel;
            write_pixel(
                pixel,
                &mut out[offset..offset + format.bytes_per_pixel],
                format.lsb_first,
            );
        }
    }

    Ok(out)
}

fn pixel_format(ctx: &X11Context, visual: Visualid, depth: u8) -> Result<PixelFormat> {
    let xformat = ctx
        .conn
        .setup()
        .pixmap_formats
        .iter()
        .find(|format| format.depth == depth)
        .with_context(|| format!("no X11 pixmap format for depth {depth}"))?;
    let visual = find_visual(ctx, visual).with_context(|| {
        format!("no X11 visual masks for visual 0x{visual:08x} at depth {depth}")
    })?;
    let bytes_per_pixel = usize::from(xformat.bits_per_pixel.div_ceil(8));
    if bytes_per_pixel == 0 || bytes_per_pixel > 4 {
        bail!(
            "unsupported X11 bits_per_pixel={} for depth {}",
            xformat.bits_per_pixel,
            depth
        );
    }

    Ok(PixelFormat {
        bits_per_pixel: xformat.bits_per_pixel,
        bytes_per_pixel,
        scanline_pad: xformat.scanline_pad,
        red_mask: visual.red_mask,
        green_mask: visual.green_mask,
        blue_mask: visual.blue_mask,
        lsb_first: ctx.conn.setup().image_byte_order == ImageOrder::LSB_FIRST,
    })
}

fn find_visual(ctx: &X11Context, visual: Visualid) -> Option<Visualtype> {
    ctx.conn
        .setup()
        .roots
        .iter()
        .flat_map(|screen| &screen.allowed_depths)
        .flat_map(|depth| &depth.visuals)
        .find(|candidate| candidate.visual_id == visual)
        .copied()
}

fn stride(width: u16, bits_per_pixel: u8, scanline_pad: u8) -> Result<usize> {
    let bits = width as usize * bits_per_pixel as usize;
    let pad = scanline_pad.max(8) as usize;
    let padded_bits = bits.div_ceil(pad) * pad;
    Ok(padded_bits / 8)
}

fn read_pixel(bytes: &[u8], bytes_per_pixel: usize, lsb_first: bool) -> u32 {
    let mut value = 0u32;
    if lsb_first {
        for (shift, byte) in bytes.iter().take(bytes_per_pixel).enumerate() {
            value |= u32::from(*byte) << (shift * 8);
        }
    } else {
        for byte in bytes.iter().take(bytes_per_pixel) {
            value = (value << 8) | u32::from(*byte);
        }
    }
    value
}

fn write_pixel(pixel: u32, out: &mut [u8], lsb_first: bool) {
    if lsb_first {
        for (shift, byte) in out.iter_mut().enumerate() {
            *byte = ((pixel >> (shift * 8)) & 0xff) as u8;
        }
    } else {
        let len = out.len();
        for (index, byte) in out.iter_mut().enumerate() {
            let shift = (len - index - 1) * 8;
            *byte = ((pixel >> shift) & 0xff) as u8;
        }
    }
}

fn extract_channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = mask >> shift;
    let value = (pixel & mask) >> shift;
    ((value * 255 + max / 2) / max) as u8
}

fn encode_channel(channel: u8, mask: u32) -> u32 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = mask >> shift;
    let value = (u32::from(channel) * max + 127) / 255;
    (value << shift) & mask
}
