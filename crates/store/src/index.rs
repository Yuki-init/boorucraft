//! The in-memory search index for one collection.
//!
//! Every file gets a number; each tag and rating gets a roaring bitmap of the numbers that have
//! it. A query is then a few bitmap ANDs, ORs and subtractions.

use std::collections::HashMap;

use boorucraft_core::{Mode, Query, Rating, TagPattern, TermKind, glob_match};
use chrono::{DateTime, Utc};
use roaring::RoaringBitmap;

use crate::Collection;

#[derive(Debug, Default)]
pub struct SearchIndex {
    /// Hash of each file number.
    hashes: Vec<String>,
    imported: Vec<DateTime<Utc>>,
    tags: HashMap<String, RoaringBitmap>,
    ratings: [RoaringBitmap; 3],
    all: RoaringBitmap,
}

impl SearchIndex {
    pub fn build(collection: &Collection) -> SearchIndex {
        let mut index = SearchIndex::default();
        for (hash, entry) in collection.files() {
            let Ok(id) = u32::try_from(index.hashes.len()) else { break };
            index.hashes.push(hash.to_string());
            index.imported.push(entry.imported);
            index.all.insert(id);
            for tag in &entry.tags {
                index.tags.entry(tag.clone()).or_default().insert(id);
            }
            if let Some(r) = entry.rating
                && let Some(set) = index.ratings.get_mut(r.index())
            {
                set.insert(id);
            }
        }
        index
    }

    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }

    /// How many files have `tag`.
    pub fn count(&self, tag: &str) -> u64 {
        self.tags.get(tag).map_or(0, RoaringBitmap::len)
    }

    /// Every tag in use with its file count, most used first.
    pub fn tag_counts(&self) -> Vec<(&str, u64)> {
        let mut out: Vec<(&str, u64)> = self.tags.iter().map(|(k, v)| (k.as_str(), v.len())).collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        out
    }

    fn term_set(&self, kind: &TermKind) -> RoaringBitmap {
        match kind {
            TermKind::Tag(TagPattern::Exact(t)) => self.tags.get(t).cloned().unwrap_or_default(),
            TermKind::Tag(TagPattern::Wildcard(p)) => {
                let mut set = RoaringBitmap::new();
                for (tag, files) in &self.tags {
                    if glob_match(p, tag) {
                        set |= files;
                    }
                }
                set
            }
            TermKind::Rating(ratings) => {
                let mut set = RoaringBitmap::new();
                for r in ratings {
                    if let Some(s) = self.ratings.get(r.index()) {
                        set |= s;
                    }
                }
                set
            }
        }
    }

    /// Hashes of the matching files, newest imported first.
    pub fn search(&self, query: &Query) -> Vec<&str> {
        let mut must: Option<RoaringBitmap> = None;
        let mut any: Option<RoaringBitmap> = None;
        let mut not = RoaringBitmap::new();
        for term in &query.terms {
            let set = self.term_set(&term.kind);
            match term.mode {
                Mode::Must => must = Some(must.map_or_else(|| set.clone(), |m| m & &set)),
                Mode::Any => any = Some(any.map_or_else(|| set.clone(), |a| a | &set)),
                Mode::Not => not |= set,
            }
        }
        let mut result = must.unwrap_or_else(|| self.all.clone());
        if let Some(a) = any {
            result &= a;
        }
        result -= not;

        let mut ids: Vec<u32> = result.iter().collect();
        ids.sort_by(|&a, &b| {
            let key = |i: u32| (self.imported.get(i as usize), self.hashes.get(i as usize));
            let (ia, ha) = key(a);
            let (ib, hb) = key(b);
            ib.cmp(&ia).then(ha.cmp(&hb))
        });
        ids.into_iter().filter_map(|i| self.hashes.get(i as usize).map(String::as_str)).collect()
    }

    /// Number of files with each rating.
    pub fn rating_counts(&self) -> [(Rating, u64); 3] {
        Rating::ALL.map(|r| (r, self.ratings.get(r.index()).map_or(0, RoaringBitmap::len)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileEntry, Kind};
    use chrono::TimeZone;

    fn collection(dir: &std::path::Path) -> Collection {
        let mut c = Collection::open(dir).unwrap();
        let files: [(&str, &[&str], Option<Rating>, i64); 5] = [
            ("a", &["cat", "sky"], Some(Rating::Safe), 1),
            ("b", &["cat", "cat_ears"], Some(Rating::Questionable), 2),
            ("c", &["dog", "sky"], Some(Rating::Explicit), 3),
            ("d", &["dog"], None, 4),
            ("e", &[], None, 5),
        ];
        for (hash, tags, rating, t) in files {
            let mut e = FileEntry::new("m".into(), Kind::Image, "png".into(), 1);
            e.tags = tags.iter().map(|t| t.to_string()).collect();
            e.rating = rating;
            e.imported = Utc.timestamp_opt(t, 0).unwrap();
            e.paths.insert(format!("{hash}.png"));
            c.insert(hash.into(), e);
        }
        c
    }

    fn search<'a>(i: &'a SearchIndex, q: &str) -> Vec<&'a str> {
        i.search(&Query::parse(q).unwrap())
    }

    #[test]
    fn booru_queries() {
        let dir = tempfile::tempdir().unwrap();
        let i = SearchIndex::build(&collection(dir.path()));
        assert_eq!(search(&i, ""), vec!["e", "d", "c", "b", "a"], "newest first");
        assert_eq!(search(&i, "cat"), vec!["b", "a"]);
        assert_eq!(search(&i, "cat sky"), vec!["a"]);
        assert_eq!(search(&i, "-cat"), vec!["e", "d", "c"]);
        assert_eq!(search(&i, "~cat ~dog -sky"), vec!["d", "b"]);
        assert_eq!(search(&i, "cat*"), vec!["b", "a"]);
        assert_eq!(search(&i, "*ears"), vec!["b"]);
        assert_eq!(search(&i, "rating:s,q"), vec!["b", "a"]);
        assert_eq!(search(&i, "sky -rating:e"), vec!["a"]);
        assert!(search(&i, "unicorn").is_empty());
        assert!(search(&i, "cat unicorn").is_empty());
        assert_eq!(i.count("sky"), 2);
        assert_eq!(i.tag_counts().first(), Some(&("cat", 2)));
    }
}
