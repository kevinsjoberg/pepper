use std::ops::Range;

use crate::buffer_position::BufferRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Insert,
    Delete,
}

#[derive(Clone, Copy)]
pub struct Edit<'a> {
    pub kind: EditKind,
    pub range: BufferRange,
    pub text: &'a str,
    pub cursor_index: u8,
}

struct EditInternal {
    pub kind: EditKind,
    pub buffer_range: BufferRange,
    pub text_range: Range<usize>,
    pub cursor_index: u8,
}

impl EditInternal {
    pub fn as_edit_ref<'a>(&self, texts: &'a str) -> Edit<'a> {
        Edit {
            kind: self.kind,
            range: self.buffer_range,
            text: &texts[self.text_range.clone()],
            cursor_index: self.cursor_index,
        }
    }
}

enum HistoryState {
    IterIndex(usize),
    InsertGroup(Range<usize>),
}

pub struct History {
    texts: String,
    edits: Vec<EditInternal>,
    group_ranges: Vec<Range<usize>>,
    state: HistoryState,
}

impl History {
    pub fn new() -> Self {
        Self {
            texts: String::new(),
            edits: Vec::new(),
            group_ranges: Vec::new(),
            state: HistoryState::IterIndex(0),
        }
    }

    pub fn clear(&mut self) {
        self.texts.clear();
        self.edits.clear();
        self.group_ranges.clear();
        self.state = HistoryState::IterIndex(0);
    }

    pub fn add_edit(&mut self, edit: Edit) {
        let current_group_len = match self.state {
            HistoryState::IterIndex(index) => {
                let edit_index = if index < self.group_ranges.len() {
                    self.group_ranges[index].start
                } else {
                    self.edits.len()
                };
                self.edits.truncate(edit_index);
                self.state = HistoryState::InsertGroup(edit_index..edit_index);
                self.group_ranges.truncate(index);
                0
            }
            HistoryState::InsertGroup(ref range) => range.end - range.start,
        };

        let merged = self.try_merge_edit(current_group_len, &edit);
        if !merged {
            if let HistoryState::InsertGroup(range) = &mut self.state {
                range.end += 1;
            }

            let texts_range_start = self.texts.len();
            self.texts.push_str(edit.text);
            self.edits.push(EditInternal {
                kind: edit.kind,
                buffer_range: edit.range,
                text_range: texts_range_start..self.texts.len(),
                cursor_index: edit.cursor_index,
            });
        }
    }

    fn try_merge_edit(&mut self, current_group_len: usize, edit: &Edit) -> bool {
        fn insert_buffer_range(edit: &mut EditInternal, range: BufferRange) {
            edit.buffer_range.from = edit.buffer_range.from.insert(range);
            edit.buffer_range.to = edit.buffer_range.to.insert(range);
        }

        fn delete_buffer_range(edit: &mut EditInternal, range: BufferRange) {
            edit.buffer_range.from = edit.buffer_range.from.delete(range);
            edit.buffer_range.to = edit.buffer_range.to.delete(range);
        }

        fn insert_text_range(edit: &mut EditInternal, start: usize, len: usize) {
            let end = start + len;
            if end <= edit.text_range.start {
                edit.text_range.start += len;
                edit.text_range.end += len;
            } else if end <= edit.text_range.end {
                edit.text_range.end += len;
            }
        }

        fn delete_text_range(edit: &mut EditInternal, start: usize, len: usize) {
            let end = start + len;
            if end <= edit.text_range.start {
                edit.text_range.start -= len;
                edit.text_range.end -= len;
            } else if end <= edit.text_range.end {
                edit.text_range.end -= len;
            }
        }

        let current_group_start = self.edits.len() - current_group_len;
        let other_edit_index = match self.edits[current_group_start..]
            .iter()
            .rposition(|e| e.cursor_index == edit.cursor_index)
        {
            Some(i) => current_group_start + i,
            None => return false,
        };
        let other_edit = &mut self.edits[other_edit_index];

        let edit_text_len = edit.text.len();

        match (other_edit.kind, edit.kind) {
            (EditKind::Insert, EditKind::Insert) => {
                // -- insert --
                //             -- insert -- (new)
                if edit.range.from == other_edit.buffer_range.to {
                    other_edit.buffer_range.to = edit.range.to;
                    self.texts.insert_str(other_edit.text_range.end, edit.text);
                    let fix_text_start = other_edit.text_range.end;
                    other_edit.text_range.end += edit_text_len;

                    for e in &mut self.edits[(other_edit_index + 1)..] {
                        insert_buffer_range(e, edit.range);
                        insert_text_range(e, fix_text_start, edit_text_len);
                    }

                    return true;
                //             -- insert --
                // -- insert -- (new)
                } else if edit.range.from == other_edit.buffer_range.from {
                    other_edit.buffer_range.to = other_edit.buffer_range.to.insert(edit.range);
                    self.texts
                        .insert_str(other_edit.text_range.start, edit.text);
                    other_edit.text_range.end += edit_text_len;

                    let fix_text_start = other_edit.text_range.start;
                    for e in &mut self.edits[(other_edit_index + 1)..] {
                        insert_buffer_range(e, edit.range);
                        insert_text_range(e, fix_text_start, edit_text_len);
                    }

                    return true;
                }
            }
            (EditKind::Delete, EditKind::Delete) => {
                // -- delete --
                //             -- delete -- (new)
                if edit.range.from == other_edit.buffer_range.from {
                    other_edit.buffer_range.to = other_edit.buffer_range.to.insert(edit.range);
                    self.texts.insert_str(other_edit.text_range.end, edit.text);
                    let fix_text_start = other_edit.text_range.end;
                    other_edit.text_range.end += edit_text_len;

                    for e in &mut self.edits[(other_edit_index + 1)..] {
                        delete_buffer_range(e, edit.range);
                        insert_text_range(e, fix_text_start, edit_text_len);
                    }

                    return true;
                //             -- delete --
                // -- delete -- (new)
                } else if edit.range.to == other_edit.buffer_range.from {
                    other_edit.buffer_range.from = edit.range.from;
                    self.texts
                        .insert_str(other_edit.text_range.start, edit.text);
                    other_edit.text_range.end += edit_text_len;

                    let fix_text_start = other_edit.text_range.start;
                    for e in &mut self.edits[(other_edit_index + 1)..] {
                        delete_buffer_range(e, edit.range);
                        insert_text_range(e, fix_text_start, edit_text_len);
                    }

                    return true;
                }
            }
            (EditKind::Insert, EditKind::Delete) => {
                // ------ insert --
                // -- delete -- (new)
                if other_edit.buffer_range.from == edit.range.from
                    && edit.range.to <= other_edit.buffer_range.to
                {
                    let deleted_text_range =
                        other_edit.text_range.start..(other_edit.text_range.start + edit_text_len);
                    if edit.text == &self.texts[deleted_text_range.clone()] {
                        other_edit.buffer_range.to = other_edit.buffer_range.to.delete(edit.range);
                        let fix_text_start = deleted_text_range.start;
                        self.texts.drain(deleted_text_range);
                        other_edit.text_range.end -= edit_text_len;

                        for e in &mut self.edits[(other_edit_index + 1)..] {
                            delete_buffer_range(e, edit.range);
                            delete_text_range(e, fix_text_start, edit_text_len);
                        }

                        return true;
                    }

                // ------ insert --
                //     -- delete -- (new)
                } else if edit.range.to == other_edit.buffer_range.to
                    && other_edit.buffer_range.from <= edit.range.from
                {
                    let deleted_text_range =
                        (other_edit.text_range.end - edit_text_len)..other_edit.text_range.end;
                    if edit.text == &self.texts[deleted_text_range.clone()] {
                        other_edit.buffer_range.to = edit.range.from;
                        let fix_text_start = deleted_text_range.start;
                        self.texts.drain(deleted_text_range);
                        other_edit.text_range.end -= edit_text_len;

                        for e in &mut self.edits[(other_edit_index + 1)..] {
                            delete_buffer_range(e, edit.range);
                            delete_text_range(e, fix_text_start, edit_text_len);
                        }

                        return true;
                    }

                // -- insert --
                // -- delete ------ (new)
                } else if edit.range.from == other_edit.buffer_range.from
                    && other_edit.buffer_range.to <= edit.range.to
                {
                    let other_text_len = other_edit.text_range.end - other_edit.text_range.start;
                    if &edit.text[..other_text_len] == &self.texts[other_edit.text_range.clone()] {
                        other_edit.kind = EditKind::Delete;
                        other_edit.buffer_range.to = edit.range.to.delete(other_edit.buffer_range);
                        self.texts.replace_range(
                            other_edit.text_range.clone(),
                            &edit.text[other_text_len..],
                        );
                        let text_len_diff = edit_text_len - other_text_len;
                        other_edit.text_range.end = other_edit.text_range.start + text_len_diff;

                        let fix_text_start = other_edit.text_range.start;
                        for e in &mut self.edits[(other_edit_index + 1)..] {
                            delete_buffer_range(e, edit.range);
                            insert_text_range(e, fix_text_start, text_len_diff);
                        }

                        return true;
                    }

                //     -- insert --
                // ------ delete -- (new)
                } else if other_edit.buffer_range.to == edit.range.to
                    && edit.range.from <= other_edit.buffer_range.from
                {
                    let other_text_len = other_edit.text_range.end - other_edit.text_range.start;
                    if &edit.text[other_text_len..] == &self.texts[other_edit.text_range.clone()] {
                        other_edit.kind = EditKind::Delete;
                        other_edit.buffer_range.to = other_edit.buffer_range.from;
                        other_edit.buffer_range.from = edit.range.from;
                        self.texts.replace_range(
                            other_edit.text_range.clone(),
                            &edit.text[..other_text_len],
                        );
                        let text_len_diff = edit_text_len - other_text_len;
                        other_edit.text_range.end = other_edit.text_range.start + text_len_diff;

                        let fix_text_start = other_edit.text_range.start;
                        for e in &mut self.edits[(other_edit_index + 1)..] {
                            delete_buffer_range(e, edit.range);
                            insert_text_range(e, fix_text_start, text_len_diff);
                        }

                        return true;
                    }
                }
            }
            _ => (),
        }

        false
    }

    pub fn commit_edits(&mut self) {
        if let HistoryState::InsertGroup(range) = &self.state {
            self.group_ranges.push(range.clone());
            self.state = HistoryState::IterIndex(self.group_ranges.len());
        }
    }

    pub fn undo_edits(&mut self) -> impl Clone + Iterator<Item = Edit> {
        self.commit_edits();

        let range = match self.state {
            HistoryState::IterIndex(ref mut index) => {
                if *index > 0 {
                    *index -= 1;
                    self.group_ranges[*index].clone()
                } else {
                    0..0
                }
            }
            _ => unreachable!(),
        };

        let texts = &self.texts;
        self.edits[range].iter().rev().map(move |e| {
            let mut edit = e.as_edit_ref(texts);
            edit.kind = match edit.kind {
                EditKind::Insert => EditKind::Delete,
                EditKind::Delete => EditKind::Insert,
            };
            edit
        })
    }

    pub fn redo_edits(&mut self) -> impl Clone + Iterator<Item = Edit> {
        self.commit_edits();

        let range = match self.state {
            HistoryState::IterIndex(ref mut index) => {
                if *index < self.group_ranges.len() {
                    let range = self.group_ranges[*index].clone();
                    *index += 1;
                    range
                } else {
                    0..0
                }
            }
            _ => unreachable!(),
        };

        let texts = &self.texts;
        self.edits[range].iter().map(move |e| e.as_edit_ref(texts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer_position::BufferPosition;

    macro_rules! buffer_range {
        ($from_line:expr, $from_column:expr => $to_line:expr, $to_column:expr) => {
            BufferRange::between(
                BufferPosition::line_col($from_line, $from_column),
                BufferPosition::line_col($to_line, $to_column),
            )
        };
    }

    #[test]
    fn commit_edits_on_emtpy_history() {
        let mut history = History::new();
        assert_eq!(0, history.undo_edits().count());
        assert_eq!(0, history.redo_edits().count());
        history.commit_edits();
        assert_eq!(0, history.redo_edits().count());
        assert_eq!(0, history.undo_edits().count());
        history.commit_edits();
        history.commit_edits();
        assert_eq!(0, history.undo_edits().count());
        assert_eq!(0, history.redo_edits().count());
    }

    #[test]
    fn edit_grouping() {
        let mut history = History::new();

        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: BufferRange::default(),
            text: "a",
            cursor_index: 0,
        });
        history.add_edit(Edit {
            kind: EditKind::Delete,
            range: BufferRange::default(),
            text: "b",
            cursor_index: 0,
        });

        assert_eq!(0, history.redo_edits().count());

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Insert, edit.kind);
        assert_eq!("b", edit.text);
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Delete, edit.kind);
        assert_eq!("a", edit.text);
        assert!(edit_iter.next().is_none());
        drop(edit_iter);

        let mut edit_iter = history.redo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Insert, edit.kind);
        assert_eq!("a", edit.text);
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Delete, edit.kind);
        assert_eq!("b", edit.text);
        assert!(edit_iter.next().is_none());
        drop(edit_iter);

        assert_eq!(0, history.redo_edits().count());

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Insert, edit.kind);
        assert_eq!("b", edit.text);
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Delete, edit.kind);
        assert_eq!("a", edit.text);
        assert!(edit_iter.next().is_none());
        drop(edit_iter);

        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: BufferRange::default(),
            text: "c",
            cursor_index: 0,
        });

        assert_eq!(0, history.redo_edits().count());

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Delete, edit.kind);
        assert_eq!("c", edit.text);
        assert!(edit_iter.next().is_none());
        drop(edit_iter);

        assert_eq!(0, history.undo_edits().count());
    }

    #[test]
    fn compress_insert_insert_edits() {
        let mut history = History::new();
        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: buffer_range!(0, 0 => 0, 3),
            text: "abc",
            cursor_index: 0,
        });
        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: buffer_range!(0, 3 => 0, 6),
            text: "def",
            cursor_index: 0,
        });

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Delete, edit.kind);
        assert_eq!("abcdef", edit.text);
        assert_eq!(buffer_range!(0, 0 => 0, 6), edit.range);
        assert!(edit_iter.next().is_none());

        let mut history = History::new();
        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: buffer_range!(0, 0 => 0, 3),
            text: "abc",
            cursor_index: 0,
        });
        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: buffer_range!(0, 0 => 0, 3),
            text: "def",
            cursor_index: 0,
        });

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Delete, edit.kind);
        assert_eq!("defabc", edit.text);
        assert_eq!(buffer_range!(0, 0 => 0, 6), edit.range);
        assert!(edit_iter.next().is_none());
    }

    #[test]
    fn compress_delete_delete_edits() {
        let mut history = History::new();
        history.add_edit(Edit {
            kind: EditKind::Delete,
            range: buffer_range!(0, 0 => 0, 3),
            text: "abc",
            cursor_index: 0,
        });
        history.add_edit(Edit {
            kind: EditKind::Delete,
            range: buffer_range!(0, 0 => 0, 3),
            text: "def",
            cursor_index: 0,
        });

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Insert, edit.kind);
        assert_eq!("abcdef", edit.text);
        assert_eq!(buffer_range!(0, 0 => 0, 6), edit.range);
        assert!(edit_iter.next().is_none());

        let mut history = History::new();
        history.add_edit(Edit {
            kind: EditKind::Delete,
            range: buffer_range!(0, 3 => 0, 6),
            text: "abc",
            cursor_index: 0,
        });
        history.add_edit(Edit {
            kind: EditKind::Delete,
            range: buffer_range!(0, 0 => 0, 3),
            text: "def",
            cursor_index: 0,
        });

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Insert, edit.kind);
        assert_eq!("defabc", edit.text);
        assert_eq!(buffer_range!(0, 0 => 0, 6), edit.range);
        assert!(edit_iter.next().is_none());
    }

    #[test]
    fn compress_insert_delete_edits() {
        // -- insert ------
        // -- delete --
        let mut history = History::new();
        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: buffer_range!(0, 0 => 0, 6),
            text: "abcdef",
            cursor_index: 0,
        });
        history.add_edit(Edit {
            kind: EditKind::Delete,
            range: buffer_range!(0, 0 => 0, 3),
            text: "abc",
            cursor_index: 0,
        });

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Delete, edit.kind);
        assert_eq!("def", edit.text);
        assert_eq!(buffer_range!(0, 0 => 0, 3), edit.range);
        assert!(edit_iter.next().is_none());

        // ------ insert --
        //     -- delete --
        let mut history = History::new();
        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: buffer_range!(0, 0 => 0, 6),
            text: "abcdef",
            cursor_index: 0,
        });
        history.add_edit(Edit {
            kind: EditKind::Delete,
            range: buffer_range!(0, 3 => 0, 6),
            text: "def",
            cursor_index: 0,
        });

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Delete, edit.kind);
        assert_eq!("abc", edit.text);
        assert_eq!(buffer_range!(0, 0 => 0, 3), edit.range);
        assert!(edit_iter.next().is_none());

        // -- insert --
        // -- delete ------
        let mut history = History::new();
        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: buffer_range!(0, 0 => 0, 3),
            text: "abc",
            cursor_index: 0,
        });
        history.add_edit(Edit {
            kind: EditKind::Delete,
            range: buffer_range!(0, 0 => 0, 6),
            text: "abcdef",
            cursor_index: 0,
        });

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Insert, edit.kind);
        assert_eq!("def", edit.text);
        assert_eq!(buffer_range!(0, 0 => 0, 3), edit.range);
        assert!(edit_iter.next().is_none());

        //     -- insert --
        // ------ delete --
        let mut history = History::new();
        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: buffer_range!(0, 3 => 0, 6),
            text: "def",
            cursor_index: 0,
        });
        history.add_edit(Edit {
            kind: EditKind::Delete,
            range: buffer_range!(0, 0 => 0, 6),
            text: "abcdef",
            cursor_index: 0,
        });

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Insert, edit.kind);
        assert_eq!("abc", edit.text);
        assert_eq!(buffer_range!(0, 0 => 0, 3), edit.range);
        assert!(edit_iter.next().is_none());
    }

    #[test]
    fn compress_multiple_edits() {
        let mut history = History::new();
        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: buffer_range!(1, 0 => 1, 1),
            text: "a",
            cursor_index: 1,
        });
        history.add_edit(Edit {
            kind: EditKind::Insert,
            range: buffer_range!(0, 0 => 0, 1),
            text: "a",
            cursor_index: 0,
        });

        let mut edit_iter = history.undo_edits();
        let edit = edit_iter.next().unwrap();
        assert_eq!(EditKind::Delete, edit.kind);
        assert_eq!(buffer_range!(1, 0 => 1, 1), edit.range);
        assert_eq!(1, edit.cursor_index);

        let edit = edit_iter.next().unwrap();
        assert_eq!(buffer_range!(0, 0 => 0, 1), edit.range);
        assert_eq!(EditKind::Delete, edit.kind);
        assert_eq!(0, edit.cursor_index);

        assert!(edit_iter.next().is_none());
    }
}
