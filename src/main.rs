use std::borrow::Cow;
use std::cmp::Ordering;

use std::collections::{HashMap, VecDeque};
use std::fmt::Display;
use std::fs::File;
use std::io::ErrorKind;

use std::path::PathBuf;
use std::{env, fs, io, mem};

use armrest::app;
use armrest::app::{Applet, Component};
use armrest::dollar::Points;

use armrest::ink::Ink;
use armrest::libremarkable::framebuffer::cgmath::Vector2;
use armrest::libremarkable::framebuffer::common::{DISPLAYHEIGHT, DISPLAYWIDTH};
use armrest::ui::canvas::Fragment;
use armrest::ui::{Side, Text, TextFragment, View, Widget};
use clap::Arg;
use once_cell::sync::Lazy;
use rusttype::Scale;

use xdg::BaseDirectories;

use font::*;
use grid_ui::*;
use hwr::*;
use text_buffer::*;

mod font;
mod grid_ui;
mod hwr;
mod text_buffer;

static BASE_DIRS: Lazy<BaseDirectories> =
    Lazy::new(|| BaseDirectories::with_prefix("armrest-editor").unwrap());

const SCREEN_HEIGHT: i32 = DISPLAYHEIGHT as i32;
const SCREEN_WIDTH: i32 = DISPLAYWIDTH as i32;
const TOP_MARGIN: i32 = 100;
const LEFT_MARGIN: i32 = 100;

const TEMPLATE_FILE: &str = "templates.json";

const HELP_TEXT: &str = include_str!("intro.md");

#[derive(Clone)]
pub enum Msg {
    SwitchTab { tab: Tab },
    Write { row: usize, ink: Ink },
    Erase { row: usize, ink: Ink },
    Swipe { towards: Side },
    Save,
    Open { path: PathBuf },
    Rename,
    New,
}

#[derive(Hash, Clone)]
struct EditChar {
    value: char,
    rendered: Option<TextFragment>,
}

// TODO: split out the margin widths.
#[derive(Hash, Clone)]
pub struct Metrics {
    height: i32,
    width: i32,
    baseline: i32,
    rows: usize,
    cols: usize,
}

impl Metrics {
    fn new(height: i32) -> Metrics {
        let scale = Scale::uniform(height as f32);
        let v_metrics = FONT.v_metrics(scale);
        let h_metrics = FONT.glyph(' ').scaled(scale).h_metrics();
        let width = h_metrics.advance_width.ceil() as i32;

        let rows = (SCREEN_HEIGHT - TOP_MARGIN * 2) / height;
        let cols = (SCREEN_WIDTH - LEFT_MARGIN * 2) / width;

        Metrics {
            height,
            width,
            baseline: v_metrics.ascent as i32 + 1,
            rows: rows as usize,
            cols: cols as usize,
        }
    }
}

#[derive(Clone)]
pub enum Tab {
    Meta {
        path_window: TextWindow,
        suggested: Vec<PathBuf>,
    },
    Edit,
    Template,
}

type Coord = (usize, usize);

#[derive(Clone)]

pub struct Carat {
    coord: Coord,
    ink: Ink,
}

#[derive(Clone)]
pub enum Selection {
    Normal,
    Single { carat: Carat },
    Range { start: Carat, end: Carat },
}

impl Default for Selection {
    fn default() -> Self {
        Selection::Normal
    }
}

#[derive(Clone)]
pub struct TextWindow {
    buffer: TextBuffer,
    grid_metrics: Metrics,
    selection: Selection,
    dimensions: Coord,
    origin: Coord,
}

impl TextWindow {
    fn new(buffer: TextBuffer, metrics: Metrics, dimensions: Coord) -> TextWindow {
        TextWindow {
            buffer,
            grid_metrics: metrics,
            selection: Selection::Normal,
            dimensions,
            origin: (0, 0),
        }
    }

    fn page_relative(&mut self, (row_d, col_d): (isize, isize)) {
        let (row, col) = &mut self.origin;
        *row = (*row as isize + row_d * self.dimensions.0 as isize).max(0) as usize;
        *col = (*col as isize + col_d * self.dimensions.1 as isize).max(0) as usize;
    }

    pub fn write(&mut self, coord: Coord, c: char) {
        self.buffer.write(coord, c);
    }

    pub fn carat(&mut self, carat: Carat) {
        // self.buffer.pad(carat.coord.0, carat.coord.1);
        self.selection = match mem::take(&mut self.selection) {
            Selection::Normal => Selection::Single { carat },
            Selection::Single { carat: original } => {
                let (start, end) = if carat.coord < original.coord {
                    (carat, original)
                } else {
                    (original, carat)
                };
                Selection::Range { start, end }
            }
            Selection::Range { .. } => {
                // Maybe eventually I'll prevent this case, but for now let's just reset.
                Selection::Normal
            }
        };
    }

    fn fragment(&self, coord: Coord) -> Option<TextFragment> {
        fragment_at(&self.buffer, coord, &self.grid_metrics)
    }

    // TODO: would be nice to enwidgetize all this!
    fn ink_row(&mut self, ink: Ink, row: usize, text_stuff: &mut TextStuff) {
        let ink_type = InkType::classify(&self.grid_metrics, ink);
        match ink_type {
            InkType::Scratch { col } => {
                let col = self.origin.1 + col;
                self.write((row, col), ' ');
                text_stuff.tentative_recognitions.clear();
            }
            InkType::Glyphs { tokens } => {
                if matches!(self.selection, Selection::Normal) {
                    for (col, ink) in tokens {
                        let col = col + self.origin.1;
                        if let Some(c) = text_stuff
                            .char_recognizer
                            .best_match(&ink_to_points(&ink, &self.grid_metrics), f32::MAX)
                        {
                            self.write((row, col), c);
                            text_stuff.record_recognition((row, col), ink, c);
                        }
                    }
                } else {
                    let ink = tokens.into_iter().next().unwrap().1;
                    match text_stuff
                        .big_recognizer
                        .best_match(&Points::normalize(&ink), f32::MAX)
                    {
                        Some('X') => {
                            if let Selection::Range { start, end } = &self.selection {
                                let trailing = self.buffer.split_off(end.coord);
                                let selection = self.buffer.split_off(start.coord);
                                text_stuff.clipboard = Some(selection);
                                self.buffer.append(trailing);
                            }
                            self.selection = Selection::Normal;
                        }
                        Some('C') => {
                            if let Selection::Range { start, end } = &self.selection {
                                // Regrettable!
                                let trailing = self.buffer.split_off(end.coord);
                                let selection = self.buffer.split_off(start.coord);
                                text_stuff.clipboard = Some(selection.clone());
                                self.buffer.append(selection);
                                self.buffer.append(trailing);
                            }
                            self.selection = Selection::Normal;
                        }
                        Some('V') => {
                            if let Selection::Single { carat } = &self.selection {
                                if let Some(buffer) = text_stuff.clipboard.take() {
                                    let trailing = self.buffer.split_off(carat.coord);
                                    self.buffer.append(buffer);
                                    self.buffer.append(trailing);
                                }
                            }
                            self.selection = Selection::Normal;
                        }
                        Some('S') => {
                            if let Selection::Range { start, end } = &self.selection {
                                self.buffer.pad(start.coord.0, start.coord.1);
                                let (lines, spaces) = if start.coord.0 == end.coord.0 {
                                    (0, end.coord.1 - start.coord.1)
                                } else {
                                    (end.coord.0 - start.coord.0, end.coord.1)
                                };
                                let mut string = String::new();
                                for _ in 0..lines {
                                    string.push('\n');
                                }
                                for _ in 0..spaces {
                                    string.push(' ');
                                }
                                self.buffer
                                    .splice(start.coord, TextBuffer::from_string(&string));
                            }
                            self.selection = Selection::Normal;
                        }
                        _ => {}
                    }
                }
            }
            InkType::Strikethrough { start, end } => {
                let start = self.origin.1 + start;
                let end = self.origin.1 + end;
                self.buffer.remove((row, start), (row, end));
                text_stuff.tentative_recognitions.clear();
            }
            InkType::Carat { col, ink } => {
                let col = self.origin.1 + col;
                self.carat(Carat {
                    coord: (row, col),
                    ink,
                });
                text_stuff.tentative_recognitions.clear();
            }
            InkType::Junk => {}
        };
    }
}

/// This stores data from a recent recognition attempt, and the number of times it was overwritten
/// within the window we maintain. Idea being, if we have to go back and rewrite a char just after
/// we wrote it, we probably guessed wrong and should use it as a template.
struct Recognition {
    coord: Coord,
    ink: Ink,
    best_char: char,
    overwrites: usize,
}

struct TextStuff {
    templates: Vec<CharTemplates>,
    char_recognizer: CharRecognizer,
    big_recognizer: CharRecognizer,
    // TODO: probably store this in TextWindow.
    tentative_recognitions: VecDeque<Recognition>,
    clipboard: Option<TextBuffer>,
}

struct Editor {
    metrics: Metrics,

    error_string: String,

    // tabs
    tab: Tab,

    // template stuff
    template_path: PathBuf,
    template_offset: usize,

    text_stuff: TextStuff,

    // text editor stuff
    path: Option<PathBuf>, // None if we haven't chosen a name yet.
    text: TextWindow,
    dirty: bool,
}

impl TextStuff {
    fn init_recognizer(&mut self, metrics: &Metrics) {
        self.char_recognizer = CharRecognizer::new(self.templates.iter().flat_map(|ct| {
            let c = ct.char;
            ct.templates
                .iter()
                .map(move |t| (ink_to_points(&t.ink, metrics), c))
        }));
        self.big_recognizer = CharRecognizer::new(
            self.templates
                .iter()
                .filter(|ct| ['X', 'C', 'V', 'S'].contains(&ct.char))
                .flat_map(|ct| {
                    let c = ct.char;
                    ct.templates
                        .iter()
                        .map(move |t| (Points::normalize(&t.ink), c))
                }),
        );
    }
}

impl Editor {
    fn load_templates(&mut self) -> io::Result<()> {
        let data = match File::open(&self.template_path) {
            Ok(file) => serde_json::from_reader(file)?,
            Err(e) if e.kind() == ErrorKind::NotFound => TemplateFile::new(&[]),
            Err(e) => return Err(e),
        };

        self.text_stuff.templates = data.to_templates(self.metrics.height);
        self.text_stuff.init_recognizer(&self.metrics);

        Ok(())
    }

    fn save_templates(&self) -> io::Result<()> {
        let file_contents = TemplateFile::new(&self.text_stuff.templates);
        serde_json::to_writer(File::create(&self.template_path)?, &file_contents)?;
        Ok(())
    }

    fn left_margin(&self) -> i32 {
        LEFT_MARGIN
    }

    fn right_margin(&self) -> i32 {
        SCREEN_WIDTH - LEFT_MARGIN - self.metrics.cols as i32 * self.metrics.width
    }

    fn draw_grid(
        &self,
        view: &mut View<Msg>,
        rows: usize,
        row_offset: usize,
        col_offset: usize,
        mut draw_label: impl FnMut(usize, View<Msg>),
        mut draw_cell: impl FnMut(usize, usize, View<Msg>),
    ) {
        const LEFT_MARGIN_BORDER: i32 = 4;
        const MARGIN_BORDER: i32 = 2;
        view.split_off(Side::Top, 2).draw(&Border {
            side: Side::Bottom,
            width: MARGIN_BORDER,
            color: 100,
            start_offset: self.left_margin() - LEFT_MARGIN_BORDER,
            end_offset: self.right_margin() - MARGIN_BORDER,
        });
        for row in row_offset..(row_offset + rows) {
            let mut line_view = view.split_off(Side::Top, self.metrics.height);
            let mut margin_view = line_view.split_off(Side::Left, LEFT_MARGIN);
            margin_view
                .split_off(Side::Right, LEFT_MARGIN_BORDER)
                .draw(&Border {
                    side: Side::Right,
                    width: LEFT_MARGIN_BORDER,
                    color: 100,
                    start_offset: 0,
                    end_offset: 0,
                });
            draw_label(row, margin_view);
            line_view
                .handlers()
                // .pad(10)
                .on_ink(|ink| Msg::Write { row, ink });
            for col in (0..self.metrics.cols).map(|c| c + col_offset) {
                let char_view = line_view.split_off(Side::Left, self.metrics.width);
                draw_cell(row, col, char_view);
            }
            line_view.draw(&Border {
                side: Side::Left,
                width: MARGIN_BORDER,
                start_offset: 0,
                end_offset: 0,
                color: 100,
            });
        }
        view.split_off(Side::Top, 2).draw(&Border {
            side: Side::Top,
            width: MARGIN_BORDER,
            color: 100,
            start_offset: self.left_margin() - LEFT_MARGIN_BORDER,
            end_offset: self.right_margin() - MARGIN_BORDER,
        });
    }

    pub fn report_error<A, E: Display>(&mut self, result: Result<A, E>) -> Option<A> {
        match result {
            Ok(a) => Some(a),
            Err(e) => {
                self.error_string = format!("Error: {}", e);
                None
            }
        }
    }
}

impl TextStuff {
    /// Our character recognizer is extremely fallible. To help improve it, we
    /// track the last few recognitions in a buffer. If we have to overwrite a
    /// recent recognition within the buffer window, we assume that the old ink
    /// should actually have been recognized as the new character. This helps
    /// bootstrap the template database; though it's still necessary to go look
    /// at the templates every once in a while and prune useless or incorrect
    /// ones.
    pub fn record_recognition(&mut self, coord: Coord, ink: Ink, best_char: char) {
        for r in &mut self.tentative_recognitions {
            // Assume we got it wrong the first time!
            if r.coord == coord {
                r.best_char = best_char;
                r.overwrites += 1;
            }
        }

        self.tentative_recognitions.push_back(Recognition {
            coord,
            ink,
            best_char,
            overwrites: 0,
        });

        while self.tentative_recognitions.len() > NUM_RECENT_RECOGNITIONS {
            if let Some(r) = self.tentative_recognitions.pop_front() {
                if r.overwrites > 0 {
                    if let Some(t) = self.templates.iter_mut().find(|c| c.char == r.best_char) {
                        t.templates.push(Template::from_ink(r.ink));
                        // self.error_string = format!(
                        //     "NB: saved template for char '{}' at coordinates {:?}",
                        //     r.best_char, r.coord
                        // );
                    }
                }
            }
        }
    }
}

fn fragment_at(
    buffer: &TextBuffer,
    (row, col): (usize, usize),
    metrics: &Metrics,
) -> Option<TextFragment> {
    let line = buffer.contents.get(row);
    line.and_then(|l| match col.cmp(&l.len()) {
        Ordering::Less => l
            .get(col)
            .map(|c| font::text_literal(metrics.height, &c.to_string()).with_weight(TEXT_WEIGHT)),
        Ordering::Equal => {
            let end_char = if row + 1 < buffer.contents.len() {
                "⏎"
            } else {
                "⌧"
            };
            Some(font::text_literal(metrics.height, end_char).with_weight(0.5))
        }
        _ => None,
    })
}

impl Widget for Editor {
    type Message = Msg;

    fn size(&self) -> Vector2<i32> {
        Vector2::new(SCREEN_WIDTH, SCREEN_HEIGHT)
    }

    fn render(&self, mut view: View<Msg>) {
        let mut header = view.split_off(Side::Top, TOP_MARGIN);
        header.split_off(Side::Left, LEFT_MARGIN);
        header.split_off(Side::Right, self.right_margin());

        match self.tab {
            Tab::Meta { .. } => {
                let head_text = Text::literal(DEFAULT_CHAR_HEIGHT, &*FONT, "Hi!");
                head_text.render_placed(header, 0.0, 0.5);
            }
            Tab::Edit => {
                let path_str = self
                    .path
                    .as_ref()
                    .map(|p| p.to_string_lossy())
                    .unwrap_or(Cow::Borrowed("unnamed file"));
                let path = self
                    .path
                    .as_ref()
                    .map(|f| f.to_string_lossy())
                    .or(env::var("HOME").ok().map(|mut s| {
                        // HOME often doesn't have a trailing slash, but multiples are OK.
                        s.push('/');
                        Cow::Owned(s)
                    }))
                    .unwrap_or(Cow::Borrowed("/"));
                let path_text = Text::builder(DEFAULT_CHAR_HEIGHT, &*FONT)
                    .message(Msg::SwitchTab {
                        tab: Tab::Meta {
                            path_window: TextWindow::new(
                                TextBuffer::from_string(&path),
                                self.metrics.clone(),
                                (1, self.metrics.cols),
                            ),
                            suggested: suggestions(&path).unwrap_or_default(),
                        },
                    })
                    .literal(&path_str)
                    .into_text();
                button("template", Msg::SwitchTab { tab: Tab::Template }, true).render_split(
                    &mut header,
                    Side::Right,
                    0.5,
                );
                button("save", Msg::Save, self.path.is_some() && self.dirty).render_split(
                    &mut header,
                    Side::Right,
                    0.5,
                );
                path_text.render_placed(header, 0.0, 0.5);
            }
            Tab::Template => {
                button("edit", Msg::SwitchTab { tab: Tab::Edit }, true).render_split(
                    &mut header,
                    Side::Right,
                    0.5,
                );
                header.leave_rest_blank();
            }
        }

        for side in [Side::Top, Side::Bottom, Side::Left, Side::Right] {
            view.handlers()
                .pad(-100)
                .on_swipe(side, Msg::Swipe { towards: side });
        }

        match &self.tab {
            Tab::Meta {
                path_window,
                suggested,
            } => {
                self.draw_grid(
                    &mut view,
                    path_window.dimensions.0,
                    path_window.origin.0,
                    path_window.origin.1,
                    |_n, _v| {},
                    |row, col, mut char_view| {
                        let coord = (row, col);
                        let ch = path_window.fragment(coord);
                        let insert_area = match &path_window.selection {
                            Selection::Normal => false,
                            Selection::Single { carat } => {
                                if coord == carat.coord {
                                    char_view.annotate(&carat.ink);
                                }
                                coord >= carat.coord
                            }
                            Selection::Range { start, end } => {
                                if coord == start.coord {
                                    char_view.annotate(&start.ink);
                                }
                                if coord == end.coord {
                                    char_view.annotate(&end.ink);
                                }
                                coord >= start.coord && coord < end.coord
                            }
                        };
                        let grid = GridCell {
                            baseline: self.metrics.baseline,
                            char: ch,
                            insert_area,
                        };
                        char_view.draw(&grid);
                    },
                );

                view.split_off(Side::Left, self.left_margin());
                view.split_off(Side::Right, self.right_margin());
                let mut buttons = view.split_off(Side::Top, TOP_MARGIN);

                for button in [
                    button("back", Msg::SwitchTab { tab: Tab::Edit }, true),
                    button("rename", Msg::Rename, true),
                    // TODO: disable if exists?
                    button("create", Msg::New, true),
                ]
                .into_iter()
                .rev()
                {
                    button.render_split(&mut buttons, Side::Right, 0.5)
                }

                buttons.leave_rest_blank();

                for s in suggested {
                    let mut suggest_view = view.split_off(Side::Top, self.metrics.height);
                    button("open", Msg::Open { path: s.clone() }, s.is_file()).render_split(
                        &mut suggest_view,
                        Side::Right,
                        0.5,
                    );
                    suggest_view.draw(&font::text_literal(
                        self.metrics.height,
                        &s.to_string_lossy(),
                    ));
                }
            }
            Tab::Edit => {
                self.draw_grid(
                    &mut view,
                    self.text.dimensions.0,
                    self.text.origin.0,
                    self.text.origin.1,
                    |_n, _v| {},
                    |row, col, mut char_view| {
                        let coord = (row, col);
                        let ch = self.text.fragment(coord);
                        let insert_area = match &self.text.selection {
                            Selection::Normal => false,
                            Selection::Single { carat } => {
                                if coord == carat.coord {
                                    char_view.annotate(&carat.ink);
                                }
                                coord >= carat.coord
                            }
                            Selection::Range { start, end } => {
                                if coord == start.coord {
                                    char_view.annotate(&start.ink);
                                }
                                if coord == end.coord {
                                    char_view.annotate(&end.ink);
                                }
                                coord >= start.coord && coord < end.coord
                            }
                        };
                        let grid = GridCell {
                            baseline: self.metrics.baseline,
                            char: ch,
                            insert_area,
                        };
                        char_view.draw(&grid);
                    },
                );
                let text = Text::literal(
                    DEFAULT_CHAR_HEIGHT,
                    &*FONT,
                    &format!(
                        "{}:{} [{}]",
                        self.text.origin.0, self.text.origin.1, self.error_string
                    ),
                );
                text.render_placed(view, 0.5, 0.5);
            }
            Tab::Template => {
                self.draw_grid(
                    &mut view,
                    self.metrics.rows,
                    self.template_offset,
                    0,
                    |row, label_view| {
                        if let Some(templates) = self.text_stuff.templates.get(row) {
                            let char_text = Text::literal(
                                self.metrics.height,
                                &*FONT,
                                &format!("{} ", templates.char),
                            );
                            char_text.render_placed(label_view, 1.0, 0.0);
                        }
                    },
                    |row, col, mut template_view| {
                        let maybe_char = self.text_stuff.templates.get(row);
                        let grid = GridCell {
                            baseline: self.metrics.baseline,
                            // char: None,
                            char: maybe_char.map(|char_data| {
                                font::text_literal(self.metrics.height, &char_data.char.to_string())
                                    .with_weight(0.2)
                            }),
                            insert_area: false,
                        };
                        if let Some(char_data) = maybe_char {
                            if let Some(template) = char_data.templates.get(col) {
                                template_view.annotate(&template.ink);
                            }
                        }
                        template_view.draw(&grid);
                    },
                );
            }
        }
    }
}

const TEXT_WEIGHT: f32 = 0.9;

/// Naively, a mark is a "scratch out" if it has a lot of ink per unit area,
/// and also isn't extremely tiny.
fn is_erase(ink: &Ink) -> bool {
    let size = ink.bounds().size();
    let area = (size.x * size.y).max(500);
    let ratio = ink.ink_len() / area as f32;
    ratio >= 0.2
}

/// What sort of ink is this?
/// The categorization here is fairly naive / hardcoded, but should do for broad classes of inputs.
enum InkType {
    // A horizontal strike through the current line: typically, delete.
    Strikethrough { start: usize, end: usize },
    // A scratch-out of a single cell: typically, replace with whitespace.
    Scratch { col: usize },
    // Something that appears to be one or more characters.
    Glyphs { tokens: HashMap<usize, Ink> },
    // A line between characters; typically represents an insertion point.
    Carat { col: usize, ink: Ink },
    // None of the above: typically, ignore.
    Junk,
}

impl InkType {
    fn tokenize(metrics: &Metrics, ink: &Ink) -> HashMap<usize, Ink> {
        // Idea: if the center of a stroke is ~this close to the margin, it's ambiguous,
        // and we decide which cell it belongs to by looking at where the neigbouring unambiguous
        // strokes end up.
        const LIMINAL_SPACE: f32 = 0.2;

        let strokes: Vec<_> = ink
            .strokes()
            .map(|s| {
                let mut i = Ink::new();
                for p in s {
                    i.push(p.x, p.y, p.z);
                }
                i.pen_up();
                i
            })
            .collect();

        let mut index_to_time_range = HashMap::new();
        for stroke in &strokes {
            let center = (stroke.centroid().x / metrics.width as f32).max(0.0);
            if (center - center.round()).abs() > LIMINAL_SPACE {
                let index = center as usize;
                let (min, max) = index_to_time_range
                    .entry(index)
                    .or_insert((f32::INFINITY, f32::NEG_INFINITY));
                *min = min.min(stroke.t_range.min);
                *max = max.max(stroke.t_range.max);
            }
        }

        let mut index_to_ink: HashMap<usize, Ink> = HashMap::new();
        for stroke in strokes {
            let center = (stroke.centroid().x / metrics.width as f32).max(0.0);
            let index = if (center - center.round()).abs() > LIMINAL_SPACE {
                center as usize
            } else {
                let right = center.round() as usize;
                if right == 0 {
                    0
                } else {
                    let left = right - 1;
                    match (
                        index_to_time_range.get(&left),
                        index_to_time_range.get(&right),
                    ) {
                        (None, None) => center as usize,
                        (Some(_), None) => left,
                        (None, Some(_)) => right,
                        (Some((_, left_max)), Some((right_min, _))) => {
                            let left_d = stroke.t_range.min - left_max;
                            let right_d = right_min - stroke.t_range.max;
                            if left_d < right_d {
                                left
                            } else {
                                right
                            }
                        }
                    }
                }
            };
            index_to_ink.entry(index).or_default().append(
                stroke.translate(-Vector2::new(index as f32 * metrics.width as f32, 0.0)),
                f32::MAX,
            );
        }

        index_to_ink
    }

    fn classify(metrics: &Metrics, ink: Ink) -> InkType {
        if ink.len() == 0 {
            return InkType::Junk;
        }

        let min_x = ink.x_range.min / metrics.width as f32;
        let max_x = ink.x_range.max / metrics.width as f32;
        let min_y = ink.y_range.min / metrics.height as f32;
        let max_y = ink.y_range.max / metrics.height as f32;

        // Roughly: a strikethrough should be a single stroke that's mostly horizontal.
        if (max_x - min_x) > 1.5 && ink.strokes().count() == 1 {
            if ink.ink_len() / (ink.x_range.max - ink.x_range.min) < 1.2 {
                return InkType::Strikethrough {
                    start: (min_x.round().max(0.0) as usize),
                    end: max_x.round().max(0.0) as usize,
                };
            } else {
                // TODO: could just be a single char!
                // Maybe fall through and handle this case as part of char splitting?
                return InkType::Junk;
            }
        }

        let center = (min_x + max_x) / 2.0;

        // Detect the carat!
        // Vertical, and very close to a cell boundary.
        if min_y < 0.1
            && max_y > 0.9
            && (max_x - min_x) < 0.3
            && (center - center.round()).abs() < 0.3
            && center.round() >= 0.0
        {
            return InkType::Carat {
                col: center.round() as usize,
                ink: ink.translate(-Vector2::new(center.round() * metrics.width as f32, 0.0)),
            };
        }

        if center < 0.0 {
            // Out of bounds!
            return InkType::Junk;
        }

        if is_erase(&ink) {
            let col = center as usize;
            return InkType::Scratch { col };
        }

        InkType::Glyphs {
            tokens: Self::tokenize(metrics, &ink),
        }
    }
}

const NUM_SUGGESTIONS: usize = 16;

fn suggestions(current_path: &str) -> io::Result<Vec<PathBuf>> {
    if !current_path.starts_with('/') {
        // All paths must be absolute.
        return Ok(vec![]);
    }
    let (dir, file) = current_path.rsplit_once('/').expect("splitting /path by /");
    let dir = if dir.is_empty() { "/" } else { dir };
    let read = fs::read_dir(dir)?;
    let results = read
        .filter_map(|r| r.ok())
        .filter_map(|de| {
            de.file_name()
                .to_str()
                .filter(|s| s.starts_with(file))
                .map(|_| de.path())
        })
        .take(NUM_SUGGESTIONS)
        .collect();
    Ok(results)
}

impl Editor {
    fn update_path_from_meta(&mut self) {
        if let Tab::Meta { path_window, .. } = &mut self.tab {
            let path_string = path_window.buffer.content_string();
            let path_buf = PathBuf::from(path_string);
            if self.path.as_ref() != Some(&path_buf) {
                self.path = Some(path_buf);
                self.dirty = true;
            }
            self.tab = Tab::Edit;
        }
    }
}

impl Applet for Editor {
    type Upstream = ();

    fn update(&mut self, message: Self::Message) -> Option<Self::Upstream> {
        match &message {
            Msg::Write { row, .. } => {
                dbg!(row);
            }
            _ => {}
        }

        match message {
            Msg::Write { row, ink } => match &mut self.tab {
                Tab::Meta {
                    path_window,
                    suggested,
                } => {
                    path_window.ink_row(ink, row, &mut self.text_stuff);
                    *suggested =
                        suggestions(&path_window.buffer.content_string()).unwrap_or_default();
                }
                Tab::Edit => {
                    self.dirty = true;
                    self.text.ink_row(ink, row, &mut self.text_stuff);
                }
                Tab::Template => {
                    let ink_type = InkType::classify(&self.metrics, ink);
                    if let Some(char_data) = self.text_stuff.templates.get_mut(row) {
                        match ink_type {
                            InkType::Strikethrough { start, end } => {
                                let line_len = char_data.templates.len();
                                for t in
                                    &mut char_data.templates[start.min(line_len)..end.min(line_len)]
                                {
                                    t.serialized.clear();
                                    t.ink.clear();
                                }
                            }
                            InkType::Scratch { col } => {
                                if let Some(prev) = char_data.templates.get_mut(col) {
                                    prev.ink.clear();
                                    prev.serialized.clear();
                                }
                            }
                            InkType::Glyphs { tokens } => {
                                for (col, ink) in tokens {
                                    if col >= char_data.templates.len() {
                                        char_data
                                            .templates
                                            .resize_with(col + 1, || Template::from_ink(Ink::new()))
                                    }
                                    let mut prev = &mut char_data.templates[col];
                                    prev.ink.append(ink, 0.5);
                                    prev.serialized = prev.ink.to_string();
                                }
                            }
                            _ => {}
                        }
                    }
                }
            },
            Msg::SwitchTab { tab } => {
                if !matches!(tab, Tab::Template) && matches!(self.tab, Tab::Template) {
                    self.report_error(self.save_templates());
                    self.text_stuff.init_recognizer(&self.metrics);
                }
                self.tab = tab;
            }
            Msg::Erase { .. } => {}
            Msg::Swipe { towards } => match self.tab {
                // TODO: abstract over the pattern here.
                Tab::Edit => {
                    let movement = match towards {
                        Side::Top => (1, 0),
                        Side::Bottom => (-1, 0),
                        Side::Left => (0, 1),
                        Side::Right => (0, -1),
                    };
                    self.text.page_relative(movement);
                }
                Tab::Template => match towards {
                    Side::Top => {
                        self.template_offset += self.metrics.rows - 1;
                    }
                    Side::Bottom => {
                        self.template_offset -= (self.metrics.rows - 1).min(self.template_offset);
                    }
                    _ => {}
                },
                Tab::Meta { .. } => {
                    // Nothing to swipe here!
                }
            },
            Msg::Open { path } => {
                if let Some(file_contents) = self.report_error(fs::read_to_string(&path)) {
                    self.text = TextWindow::new(
                        TextBuffer::from_string(&file_contents),
                        self.metrics.clone(),
                        (self.metrics.rows, self.metrics.cols),
                    );
                    self.path = Some(path);
                    self.tab = Tab::Edit;
                    self.dirty = false;
                    self.text_stuff.tentative_recognitions.clear();
                }
            }
            Msg::Rename => {
                self.update_path_from_meta();
            }
            Msg::New => {
                self.update_path_from_meta();
                self.text = TextWindow::new(
                    TextBuffer::empty(),
                    self.metrics.clone(),
                    (self.metrics.rows, self.metrics.cols),
                );
            }
            Msg::Save => {
                if let Some(path) = &self.path {
                    let write_result = std::fs::write(path, self.text.buffer.content_string());
                    if write_result.is_ok() {
                        self.dirty = false;
                    }
                    self.report_error(write_result);
                }
            }
        }

        None
    }
}

fn button(text: &str, msg: Msg, active: bool) -> Text<Msg> {
    let builder = Text::builder(DEFAULT_CHAR_HEIGHT, &*FONT).literal("    ");
    let builder = if active {
        builder.message(msg).weight(TEXT_WEIGHT)
    } else {
        builder.weight(0.5)
    };
    builder.literal(text).into_text()
}

const NUM_RECENT_RECOGNITIONS: usize = 16;

fn main() {
    let mut app = app::App::new();

    let args = clap::Command::new("armrest-editor")
        .arg(Arg::new("file"))
        .get_matches();

    let file_string = if let Some(os_path) = args.value_of_os("file") {
        std::fs::read_to_string(os_path).expect("Unable to read specified file!")
    } else {
        HELP_TEXT.to_string() // Unnecessary cost, but not a big deal?
    };

    let template_path = BASE_DIRS
        .place_data_file(TEMPLATE_FILE)
        .expect("placing the template data file");

    let metrics = Metrics::new(DEFAULT_CHAR_HEIGHT);

    let dimensions = (metrics.rows, metrics.cols);

    let mut widget = Editor {
        path: None,
        template_path,
        metrics: metrics.clone(),
        error_string: "".to_string(),
        tab: Tab::Edit,
        template_offset: 0,
        text_stuff: TextStuff {
            templates: vec![],
            char_recognizer: CharRecognizer::new([]),
            big_recognizer: CharRecognizer::new([]),
            tentative_recognitions: VecDeque::with_capacity(NUM_RECENT_RECOGNITIONS),
            clipboard: None,
        },
        text: TextWindow::new(TextBuffer::from_string(&file_string), metrics, dimensions),
        dirty: false,
    };

    let load_result = widget.load_templates();
    widget.report_error(load_result);

    app.run(&mut Component::new(widget))
}
