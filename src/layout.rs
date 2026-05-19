use crate::config::AppConfig;
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

pub fn compute(
    screen_width: u16,
    screen_height: u16,
    windows: &[WindowInfo],
    config: &AppConfig,
) -> Layout {
    let count = windows.len().max(1);
    let margin = config
        .layout
        .margin
        .min(screen_width / 2)
        .min(screen_height / 2);
    let gap = if count <= 3 {
        config.layout.gap_small
    } else {
        config.layout.gap
    };
    let top_meta_height = if config.ui.show_workspace_number {
        config.layout.top_meta_height
    } else {
        config.layout.padding
    };
    let label_height = if config.ui.show_program_name {
        config.layout.label_height
    } else {
        0
    };
    let padding = config.layout.padding;
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
        .map(|(index, _window)| {
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
                y: y + top_meta_height as i16,
                width: cell_width.saturating_sub(padding * 2),
                height: cell_height
                    .saturating_sub(top_meta_height)
                    .saturating_sub(label_height)
                    .saturating_sub(padding),
            };
            LayoutItem {
                cell,
                preview: preview_area,
            }
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
