use crate::{text_literal, Metrics, Vector2};
use armrest::libremarkable::framebuffer::common::color;
use armrest::libremarkable::framebuffer::FramebufferIO;
use armrest::ui::{Cached, Canvas, Fragment, Side, TextFragment, View};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

const GRID_LINE_COLOR: color = color::GRAY(40);
const GUIDE_LINE_COLOR: color = color::GRAY(40);

pub type Coord = (usize, usize);

// The width of the padding we put around a drawn grid. May or may not be coloured in.
pub const GRID_BORDER: i32 = 4;

fn fill(canvas: &mut Canvas, xs: Range<i32>, ys: Range<i32>) {
    for y in ys {
        for x in xs.clone() {
            canvas.write(x, y, GRID_LINE_COLOR);
        }
    }
}
fn line(canvas: &mut Canvas, xs: Range<i32>, ys: Range<i32>, width: i32) {
    // grid remnant
    for y in ys {
        for x in xs.clone().step_by(width as usize) {
            canvas.write(x, y, GRID_LINE_COLOR);
        }
    }
}

#[derive(Hash)]
pub struct GridBorder {
    pub side: Side,
    pub width: i32,
}

impl Fragment for GridBorder {
    fn draw(&self, canvas: &mut Canvas) {
        let size = canvas.bounds().size();

        match self.side {
            Side::Left => {
                fill(canvas, 0..size.x, 0..size.y);
            }
            Side::Right => {
                fill(canvas, 0..size.x, 0..size.y);
            }
            Side::Top => {
                fill(canvas, 0..size.x, 0..2);
                line(canvas, 0..size.x, 2..size.y, self.width);
            }
            Side::Bottom => {
                let y = size.y - 2;
                line(canvas, 0..size.x, 0..y, self.width);
                fill(canvas, 0..size.x, y..size.y);
            }
        }
    }
}

#[derive(Hash)]
pub struct GridCell {
    pub baseline: i32,
    pub char: Option<TextFragment>,
    pub insert_area: bool,
}

impl Fragment for GridCell {
    fn draw(&self, canvas: &mut Canvas) {
        if let Some(c) = &self.char {
            c.draw(canvas);
        }

        let base_pixel = canvas.bounds().top_left;
        let size = canvas.bounds().size();
        let fb = canvas.framebuffer();

        let mut darken = move |x: i32, y: i32, color: color| {
            let pixel = base_pixel + Vector2::new(x, y);
            let read_pixel = pixel.map(|c| c as u32);
            let [r0, g0, b0] = fb.read_pixel(read_pixel).to_rgb8();
            let [r1, g1, b1] = color.to_rgb8();
            let combined = color::RGB(r0.min(r1), g0.min(g1), b0.min(b1));
            fb.write_pixel(pixel, combined);
        };

        let top_line = self.baseline - size.y * 3 / 4;
        let mid_line = self.baseline - size.y * 2 / 4;
        let bottom_line = self.baseline - size.y * 1 / 4;
        for y in 0..size.y {
            darken(0, y, GRID_LINE_COLOR);
        }
        for x in 1..size.x {
            darken(x, top_line, GUIDE_LINE_COLOR);
            darken(x, mid_line, GUIDE_LINE_COLOR);
            darken(x, bottom_line, GUIDE_LINE_COLOR);
            darken(x, self.baseline, GRID_LINE_COLOR);
            darken(x, self.baseline + 1, GRID_LINE_COLOR);
            if self.insert_area {
                darken(x, self.baseline + 2, GRID_LINE_COLOR);
                darken(x, self.baseline + 3, GRID_LINE_COLOR);
            }
        }
    }
}

pub struct Atlas {
    metrics: Metrics,
    cache: RefCell<HashMap<(Option<char>, bool, bool), Rc<Cached<GridCell>>>>,
}

impl Atlas {
    pub fn new(metrics: Metrics) -> Atlas {
        Atlas {
            metrics,
            cache: RefCell::new(Default::default()),
        }
    }

    fn fresh_cell(
        &self,
        char: Option<char>,
        selected: bool,
        background: bool,
    ) -> Rc<Cached<GridCell>> {
        let weight = if background { 0.3 } else { 0.9 };
        Rc::new(Cached::new(GridCell {
            baseline: self.metrics.baseline,
            char: char
                .map(|c| text_literal(self.metrics.height, &c.to_string()).with_weight(weight)),
            insert_area: selected,
        }))
    }

    pub fn get_cell(
        &self,
        char: Option<char>,
        selected: bool,
        background: bool,
    ) -> Rc<Cached<GridCell>> {
        if let Ok(mut cache) = self.cache.try_borrow_mut() {
            let value = cache
                .entry((char, selected, background))
                .or_insert(self.fresh_cell(char, selected, background));
            Rc::clone(value)
        } else {
            // Again, shouldn't be common, but it's good to be prepared!
            self.fresh_cell(char, selected, background)
        }
    }
}

// TODO: consider making this a widget?
pub fn draw_grid<T>(
    mut view: View<T>,
    metrics: &Metrics,
    dimensions: Coord,
    mut on_row: impl FnMut(usize, &mut View<T>),
    mut draw_cell: impl FnMut(usize, usize, View<T>),
) {
    let (rows, cols) = dimensions;
    // TODO: fit to space provided?
    const LEFT_MARGIN_BORDER: i32 = 4;
    const MARGIN_BORDER: i32 = 2;

    // TODO: put this in armrest
    let section_height = metrics.height as f32 / 4.0;
    let baseline_grid_offset = metrics.baseline as f32 % section_height;

    let top_height = (section_height - baseline_grid_offset).ceil() as i32 + 2;
    let bottom_height = baseline_grid_offset.floor() as i32 + 2;
    let left_width = 1; // NB: has a pixel of line in the cell already
    let right_width = 2;

    let height = rows as i32 * metrics.height + top_height + bottom_height;
    let width = cols as i32 * metrics.width + left_width + right_width;
    let remaining = view.size();
    view.split_off(Side::Right, (remaining.x - width).max(0));
    view.split_off(Side::Bottom, (remaining.y - height).max(0));

    // let view = view.split_off(Side::Left, cols as usize * metrics.width + GRID_BORDER * 2);

    view.split_off(Side::Left, left_width).draw(&GridBorder {
        width: metrics.width,
        side: Side::Left,
    });
    view.split_off(Side::Right, right_width).draw(&GridBorder {
        width: metrics.width,
        side: Side::Right,
    });
    view.split_off(
        Side::Top,
        (section_height - baseline_grid_offset).ceil() as i32 + 2,
    )
    .draw(&GridBorder {
        width: metrics.width,
        side: Side::Top,
    });
    view.split_off(Side::Bottom, bottom_height)
        .draw(&GridBorder {
            width: metrics.width,
            side: Side::Bottom,
        });
    for row in 0..rows {
        let mut line_view = view.split_off(Side::Top, metrics.height);
        on_row(row, &mut line_view);
        for col in 0..cols {
            let char_view = line_view.split_off(Side::Left, metrics.width);
            draw_cell(row, col, char_view);
        }
    }
}
