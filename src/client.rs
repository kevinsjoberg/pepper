use std::path::PathBuf;

use crate::{
    buffer::BufferContent, buffer_position::BufferRange, config::Config, cursor::Cursor,
    editor::EditorOperation, mode::Mode,
};

pub struct Client {
    pub config: Config,

    pub mode: Mode,

    pub path: Option<PathBuf>,
    pub buffer: BufferContent,

    pub scroll: usize,
    pub cursors: Vec<Cursor>,
    pub search_ranges: Vec<BufferRange>,

    pub has_focus: bool,
    pub input: String,
}

impl Client {
    pub fn new() -> Self {
        Self {
            config: Config::default(),

            mode: Mode::default(),

            path: None,
            buffer: BufferContent::from_str(""),

            scroll: 0,
            cursors: Vec::new(),
            search_ranges: Vec::new(),

            has_focus: true,
            input: String::new(),
        }
    }

    pub fn on_editor_operation(&mut self, operation: EditorOperation) {
        match operation {
            EditorOperation::Content(text) => self.buffer = BufferContent::from_str(text),
            EditorOperation::Path(path) => self.path = path.map(|p| p.into()),
            EditorOperation::Mode(mode) => self.mode = mode,
            EditorOperation::Insert(position, text) => {
                self.buffer.insert_text(position, text);
            }
            EditorOperation::Delete(range) => {
                self.buffer.delete_range(range);
            }
            EditorOperation::ClearCursors => self.cursors.clear(),
            EditorOperation::Cursor(cursor) => self.cursors.push(cursor),
            EditorOperation::Search(search) => {
                self.search_ranges.clear();
                self.buffer
                    .find_search_ranges(&search[..], &mut self.search_ranges);
            }
        }
    }
}
