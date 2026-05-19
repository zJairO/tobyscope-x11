use crate::windows::WindowInfo;

#[derive(Debug, Clone, Copy, Default)]
pub struct Rect {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn contains(self, x: i16, y: i16) -> bool {
        let right = self.x.saturating_add(self.width as i16);
        let bottom = self.y.saturating_add(self.height as i16);
        x >= self.x && y >= self.y && x < right && y < bottom
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutItem {
    pub cell: Rect,
    pub preview: Rect,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub columns: usize,
    pub items: Vec<LayoutItem>,
}

pub fn compute(screen_width: u16, screen_height: u16, windows: &[WindowInfo]) -> Layout {
    let count = windows.len().max(1);
    let margin = 24u16.min(screen_width / 10).min(screen_height / 10);
    let gap = if count <= 3 { 12u16 } else { 18u16 };
    let label_height = 34u16;
    let padding = 12u16;
    let (columns, rows) = choose_grid(count, screen_width, screen_height);

    let total_gap_x = gap.saturating_mul(columns.saturating_sub(1) as u16);
    let total_gap_y = gap.saturating_mul(rows.saturating_sub(1) as u16);
    let usable_width = screen_width
        .saturating_sub(margin.saturating_mul(2))
        .saturating_sub(total_gap_x)
        .max(1);
    let usable_height = screen_height
        .saturating_sub(margin.saturating_mul(2))
        .saturating_sub(total_gap_y)
        .max(1);

    let cell_width = (usable_width / columns as u16).max(1);
    let cell_height = (usable_height / rows as u16).max(1);
    let grid_width = cell_width
        .saturating_mul(columns as u16)
        .saturating_add(total_gap_x);
    let grid_height = cell_height
        .saturating_mul(rows as u16)
        .saturating_add(total_gap_y);
    let start_x = ((screen_width.saturating_sub(grid_width)) / 2) as i16;
    let start_y = ((screen_height.saturating_sub(grid_height)) / 2) as i16;

    let items = windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            let col = index % columns;
            let row = index / columns;
            let x = start_x + (col as i16 * (cell_width + gap) as i16);
            let y = start_y + (row as i16 * (cell_height + gap) as i16);
            let cell = Rect {
                x,
                y,
                width: cell_width,
                height: cell_height,
            };

            let preview_area = Rect {
                x: x + padding as i16,
                y: y + padding as i16,
                width: cell_width.saturating_sub(padding * 2),
                height: cell_height
                    .saturating_sub(label_height)
                    .saturating_sub(padding * 2),
            };
            let preview = fit_aspect(
                preview_area,
                window.geometry.width.max(1),
                window.geometry.height.max(1),
            );

            LayoutItem { cell, preview }
        })
        .collect();

    Layout { columns, items }
}

pub fn hit_test(layout: &Layout, x: i16, y: i16) -> Option<usize> {
    layout
        .items
        .iter()
        .position(|item| item.cell.contains(x, y))
}

fn choose_grid(count: usize, screen_width: u16, screen_height: u16) -> (usize, usize) {
    match count {
        0 | 1 => return (1, 1),
        2 => return (2, 1),
        3 => return (3, 1),
        _ => {}
    }

    let aspect = screen_width as f32 / screen_height.max(1) as f32;
    let mut columns = ((count as f32 * aspect).sqrt().ceil() as usize).max(1);
    columns = columns.min(count);
    let rows = count.div_ceil(columns).max(1);
    (columns, rows)
}

fn fit_aspect(area: Rect, source_width: u16, source_height: u16) -> Rect {
    if area.width == 0 || area.height == 0 {
        return area;
    }

    let area_ratio = area.width as f32 / area.height as f32;
    let source_ratio = source_width as f32 / source_height as f32;
    let (width, height) = if source_ratio > area_ratio {
        let width = area.width;
        let height = ((width as f32 / source_ratio).round() as u16)
            .max(1)
            .min(area.height);
        (width, height)
    } else {
        let height = area.height;
        let width = ((height as f32 * source_ratio).round() as u16)
            .max(1)
            .min(area.width);
        (width, height)
    };

    Rect {
        x: area.x + ((area.width - width) / 2) as i16,
        y: area.y + ((area.height - height) / 2) as i16,
        width,
        height,
    }
}
