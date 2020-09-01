use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};

#[derive(Default)]
pub struct SelectEntry {
    pub name: String,
}

#[derive(Default)]
struct FilteredEntry {
    pub entry_index: usize,
    pub score: i64,
}

#[derive(Default)]
pub struct SelectEntryCollection {
    pub selected_index: usize,
    len: usize,
    entries: Vec<SelectEntry>,
    filtered: Vec<FilteredEntry>,
    filter: String,
    matcher: SkimMatcherV2,
}

impl SelectEntryCollection {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn clear(&mut self) {
        self.selected_index = 0;
        self.len = 0;
        self.filtered.clear();
    }

    pub fn add(&mut self, name: &str) {
        let entry = if self.len < self.entries.len() {
            &mut self.entries[self.len]
        } else {
            self.entries.push(SelectEntry::default());
            self.len = self.entries.len();
            &mut self.entries[self.len - 1]
        };

        entry.name.clear();
        entry.name.push_str(name);
        self.filter();
    }

    pub fn set_filter(&mut self, filter: &str) {
        self.filter.clear();
        self.filter.push_str(filter);
        self.filter();
    }

    fn filter(&mut self) {
        self.filtered.clear();
        let filter = &self.filter[..];
        for (i, e) in self.entries.iter().take(self.len).enumerate() {
            if let Some(score) = self.matcher.fuzzy_match(&e.name[..], filter) {
                self.filtered.push(FilteredEntry {
                    entry_index: i,
                    score,
                });
            }
        }

        self.filtered.sort_unstable_by_key(|f| f.score);
    }

    pub fn filtered_entries(&self) -> impl Iterator<Item = &SelectEntry> {
        self.filtered
            .iter()
            .map(move |f| &self.entries[f.entry_index])
    }
}
