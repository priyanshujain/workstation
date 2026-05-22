use ratatui::prelude::Rect;

pub fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let y = area.height.saturating_sub(height) / 2;
    let width = area.width * percent_x / 100;
    let x = (area.width.saturating_sub(width)) / 2;
    Rect::new(x, y, width, height)
}
