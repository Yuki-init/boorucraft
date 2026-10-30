//! The in-memory search index for one collection.
//!
//! Every file gets a number and a [`Row`] holding the columns metatags filter and sort on. Each
//! tag and rating gets a roaring bitmap of the numbers that have it. A query is then a few bitmap
//! ANDs, ORs and subtractions; metatags scan the rows.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use boorucraft_core::{DateField, Mode, NumField, Order, OrderKey, Query, Rating, TagPattern, TermKind, glob_match};
use chrono::{DateTime, Utc};
use roaring::RoaringBitmap;

use crate::{Collection, FileEntry, Kind};

/// Facts that depend on more than one file, possibly in other collections, worked out by the
/// caller (duplicate detection lives outside the store).
#[derive(Debug, Clone, Default)]
pub struct Links {
    /// Hashes of files that have a near-duplicate.
    pub dupes: HashSet<String>,
}

/// The searchable and sortable columns of one file.
#[derive(Debug, Clone)]
pub struct Row {
    pub hash: String,
    md5: String,
    format: String,
    kind: Kind,
    size: u64,
    width: Option<u32>,
    height: Option<u32>,
    animated: bool,
    missing: bool,
    /// When the picture was taken, else when it was imported.
    taken: DateTime<Utc>,
    pub imported: DateTime<Utc>,
    tagcount: u32,
    /// Lowercase file name of the first path.
    name: String,
    /// Lowercase relative paths.
    paths: Vec<String>,
    source: Option<String>,
}

impl Row {
    fn new(hash: &str, e: &FileEntry) -> Row {
        let paths: Vec<String> = e.paths.iter().map(|p| p.to_lowercase()).collect();
        let name = paths.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).unwrap_or_default();
        Row {
            hash: hash.to_string(),
            md5: e.md5.to_lowercase(),
            format: e.format.clone(),
            kind: e.kind,
            size: e.size,
            width: e.width,
            height: e.height,
            animated: e.animated,
            missing: e.missing,
            taken: e.taken.unwrap_or(e.imported),
            imported: e.imported,
            tagcount: u32::try_from(e.tags.len()).unwrap_or(u32::MAX),
            name,
            paths,
            source: e.source.as_ref().map(|s| s.to_lowercase()),
        }
    }

    fn number(&self, field: NumField) -> Option<f64> {
        let (w, h) = (self.width.map(f64::from), self.height.map(f64::from));
        match field {
            NumField::Width => w,
            NumField::Height => h,
            NumField::Ratio => match (w, h) {
                (Some(w), Some(h)) if h > 0.0 => Some(w / h),
                _ => None,
            },
            NumField::Mpixels => Some(w? * h? / 1_000_000.0),
            NumField::Filesize => Some(self.size as f64),
            NumField::Tagcount => Some(f64::from(self.tagcount)),
        }
    }

    /// Does this row match a metatag? (Tags and ratings use bitmaps instead.)
    fn matches(&self, kind: &TermKind, now: DateTime<Utc>, links: &Links) -> bool {
        match kind {
            TermKind::Tag(_) | TermKind::Rating(_) => false,
            TermKind::Number(field, range) => self.number(*field).is_some_and(|v| range.contains(v)),
            TermKind::Date(field, spec) => {
                let t = match field {
                    DateField::Taken => self.taken,
                    DateField::Imported => self.imported,
                };
                let (from, until) = spec.resolve(now);
                from.is_none_or(|f| t >= f) && until.is_none_or(|u| t < u)
            }
            TermKind::Format(formats) => formats.contains(&self.format),
            TermKind::Kind(kinds) => {
                let k = match self.kind {
                    Kind::Image => "image",
                    Kind::Video => "video",
                    Kind::Other => "other",
                };
                kinds.iter().any(|x| x == k)
            }
            TermKind::Animated(want) => self.animated == *want,
            TermKind::Missing(want) => self.missing == *want,
            TermKind::Dupes(want) => links.dupes.contains(&self.hash) == *want,
            TermKind::Path(m) => self.paths.iter().any(|p| m.matches(p)),
            TermKind::Md5(m) => self.md5 == *m,
            TermKind::Hash(prefix) => self.hash.starts_with(prefix.as_str()),
            TermKind::Source(None) => self.source.is_none(),
            TermKind::Source(Some(m)) => self.source.as_deref().is_some_and(|s| m.matches(s)),
        }
    }
}

/// Sorts search results; the same order whichever collections they come from.
///
/// `None` (no `order:`) is newest imported first. Files without the value (no width, say) go
/// last either way. Ties fall back to newest imported, then the hash, so the order is stable.
pub fn compare_rows(order: Option<&Order>, seed: u64, a: &Row, b: &Row) -> Ordering {
    let default = || b.imported.cmp(&a.imported).then_with(|| a.hash.cmp(&b.hash));
    let Some(order) = order else { return default() };
    let dir = |o: Ordering| if order.descending { o.reverse() } else { o };
    let nums = |field| match (a.number(field), b.number(field)) {
        (Some(x), Some(y)) => dir(x.total_cmp(&y)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    let first = match order.key {
        OrderKey::Imported => dir(a.imported.cmp(&b.imported)),
        OrderKey::Date => dir(a.taken.cmp(&b.taken)),
        OrderKey::Filesize => nums(NumField::Filesize),
        OrderKey::Width => nums(NumField::Width),
        OrderKey::Height => nums(NumField::Height),
        OrderKey::Mpixels => nums(NumField::Mpixels),
        OrderKey::Ratio => nums(NumField::Ratio),
        OrderKey::Tagcount => nums(NumField::Tagcount),
        OrderKey::Name => dir(a.name.cmp(&b.name).then_with(|| a.paths.cmp(&b.paths))),
        OrderKey::Path => dir(a.paths.cmp(&b.paths)),
        OrderKey::Random => shuffle_key(seed, &a.hash).cmp(&shuffle_key(seed, &b.hash)),
    };
    first.then_with(default)
}

/// A pseudo-random number per file and seed (FNV-1a, then a SplitMix64 finish).
fn shuffle_key(seed: u64, hash: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ seed;
    for b in hash.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

#[derive(Debug, Default)]
pub struct SearchIndex {
    rows: Vec<Row>,
    tags: HashMap<String, RoaringBitmap>,
    ratings: [RoaringBitmap; 3],
    all: RoaringBitmap,
}

impl SearchIndex {
    pub fn build(collection: &Collection) -> SearchIndex {
        let mut index = SearchIndex::default();
        for (hash, entry) in collection.files() {
            let Ok(id) = u32::try_from(index.rows.len()) else { break };
            index.rows.push(Row::new(hash, entry));
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
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
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

    fn term_set(&self, kind: &TermKind, now: DateTime<Utc>, links: &Links) -> RoaringBitmap {
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
            other => (0u32..).zip(&self.rows).filter(|(_, row)| row.matches(other, now, links)).map(|(id, _)| id).collect(),
        }
    }

    /// The rows matching a query, unsorted (sort them with [`compare_rows`]).
    pub fn matching_rows(&self, query: &Query, now: DateTime<Utc>, links: &Links) -> Vec<&Row> {
        let mut must: Option<RoaringBitmap> = None;
        let mut any: Option<RoaringBitmap> = None;
        let mut not = RoaringBitmap::new();
        for term in &query.terms {
            let set = self.term_set(&term.kind, now, links);
            match term.mode {
                Mode::Must => must = Some(must.map_or(set.clone(), |m| m & &set)),
                Mode::Any => any = Some(any.map_or(set.clone(), |a| a | &set)),
                Mode::Not => not |= set,
            }
        }
        let mut result = must.unwrap_or_else(|| self.all.clone());
        if let Some(a) = any {
            result &= a;
        }
        result -= not;
        result.iter().filter_map(|i| self.rows.get(i as usize)).collect()
    }

    /// Hashes of the matching files in the query's order (default: newest imported first).
    pub fn search_at(&self, query: &Query, now: DateTime<Utc>, links: &Links) -> Vec<&str> {
        let mut rows = self.matching_rows(query, now, links);
        rows.sort_by(|a, b| compare_rows(query.order.as_ref(), query.seed, a, b));
        rows.into_iter().map(|r| r.hash.as_str()).collect()
    }

    /// [`Self::search_at`] now.
    pub fn search(&self, query: &Query, links: &Links) -> Vec<&str> {
        self.search_at(query, Utc::now(), links)
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

    struct F {
        hash: &'static str,
        tags: &'static [&'static str],
        rating: Option<Rating>,
        imported: i64,
        size: (u32, u32),
        bytes: u64,
        format: &'static str,
        path: &'static str,
    }

    const FILES: [F; 5] = [
        F {
            hash: "a1a1",
            tags: &["cat", "sky"],
            rating: Some(Rating::Safe),
            imported: 1,
            size: (1920, 1080),
            bytes: 500_000,
            format: "jpeg",
            path: "Holiday 2023/a.jpg",
        },
        F {
            hash: "b2b2",
            tags: &["cat", "cat_ears"],
            rating: Some(Rating::Questionable),
            imported: 2,
            size: (800, 600),
            bytes: 3_000_000,
            format: "png",
            path: "b.png",
        },
        F {
            hash: "c3c3",
            tags: &["dog", "sky"],
            rating: Some(Rating::Explicit),
            imported: 3,
            size: (1080, 1920),
            bytes: 90_000,
            format: "webp",
            path: "pets/c.webp",
        },
        F { hash: "d4d4", tags: &["dog"], rating: None, imported: 4, size: (0, 0), bytes: 7_000_000, format: "mp4", path: "pets/D.mp4" },
        F { hash: "e5e5", tags: &[], rating: None, imported: 5, size: (4000, 3000), bytes: 2_000_000, format: "jpeg", path: "e.jpg" },
    ];

    fn day(n: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap() + chrono::TimeDelta::days(n)
    }

    fn collection(dir: &std::path::Path) -> Collection {
        let mut c = Collection::open(dir).unwrap();
        for f in FILES {
            let kind = if f.format == "mp4" { Kind::Video } else { Kind::Image };
            let mut e = FileEntry::new(format!("{:0>32}", f.hash), kind, f.format.into(), f.bytes);
            e.tags = f.tags.iter().map(|t| t.to_string()).collect();
            e.rating = f.rating;
            e.imported = day(f.imported * 30);
            e.taken = (f.hash != "e5e5").then(|| day(-f.imported));
            if f.size.0 > 0 {
                (e.width, e.height) = (Some(f.size.0), Some(f.size.1));
            }
            e.animated = f.format == "webp";
            e.missing = f.hash == "b2b2";
            if f.hash == "a1a1" {
                e.source = Some("https://www.pixiv.net/artworks/1".into());
            }
            e.paths.insert(f.path.into());
            c.insert(f.hash.into(), e);
        }
        c
    }

    fn search<'a>(i: &'a SearchIndex, q: &str) -> Vec<&'a str> {
        let links = Links { dupes: ["a1a1".to_string(), "c3c3".to_string()].into() };
        i.search_at(&Query::parse(q).unwrap(), day(200), &links)
    }

    #[test]
    fn booru_queries() {
        let dir = tempfile::tempdir().unwrap();
        let i = SearchIndex::build(&collection(dir.path()));
        assert_eq!(search(&i, ""), vec!["e5e5", "d4d4", "c3c3", "b2b2", "a1a1"], "newest first");
        assert_eq!(search(&i, "cat"), vec!["b2b2", "a1a1"]);
        assert_eq!(search(&i, "cat sky"), vec!["a1a1"]);
        assert_eq!(search(&i, "-cat"), vec!["e5e5", "d4d4", "c3c3"]);
        assert_eq!(search(&i, "~cat ~dog -sky"), vec!["d4d4", "b2b2"]);
        assert_eq!(search(&i, "cat*"), vec!["b2b2", "a1a1"]);
        assert_eq!(search(&i, "*ears"), vec!["b2b2"]);
        assert_eq!(search(&i, "rating:s,q"), vec!["b2b2", "a1a1"]);
        assert_eq!(search(&i, "sky -rating:e"), vec!["a1a1"]);
        assert!(search(&i, "unicorn").is_empty());
        assert!(search(&i, "cat unicorn").is_empty());
        assert_eq!(i.count("sky"), 2);
        assert_eq!(i.tag_counts().first(), Some(&("cat", 2)));
    }

    #[test]
    fn metatags() {
        let dir = tempfile::tempdir().unwrap();
        let i = SearchIndex::build(&collection(dir.path()));
        assert_eq!(search(&i, "width:>=1920"), vec!["e5e5", "a1a1"], "files without a width never match");
        assert_eq!(search(&i, "-width:>=1920"), vec!["d4d4", "c3c3", "b2b2"]);
        assert_eq!(search(&i, "height:1000..2000"), vec!["c3c3", "a1a1"]);
        assert_eq!(search(&i, "ratio:16:9"), vec!["a1a1"]);
        assert_eq!(search(&i, "ratio:<1"), vec!["c3c3"]);
        assert_eq!(search(&i, "mpixels:>10"), vec!["e5e5"]);
        assert_eq!(search(&i, "filesize:>2MB"), vec!["d4d4", "b2b2"]);
        assert_eq!(search(&i, "type:jpg"), vec!["e5e5", "a1a1"]);
        assert_eq!(search(&i, "kind:video"), vec!["d4d4"]);
        assert_eq!(search(&i, "animated:yes"), vec!["c3c3"]);
        assert_eq!(search(&i, "missing:yes"), vec!["b2b2"]);
        assert_eq!(search(&i, "tagcount:0"), vec!["e5e5"]);
        assert_eq!(search(&i, "tagcount:>=2"), vec!["c3c3", "b2b2", "a1a1"]);
        assert_eq!(search(&i, "path:\"holiday 2023\""), vec!["a1a1"]);
        assert_eq!(search(&i, "path:pets/*"), vec!["d4d4", "c3c3"]);
        assert_eq!(search(&i, "source:*pixiv*"), vec!["a1a1"]);
        assert_eq!(search(&i, "-source:none"), vec!["a1a1"]);
        assert_eq!(search(&i, "hash:c3c3"), vec!["c3c3"]);
        assert_eq!(search(&i, &format!("md5:{:0>32}", "d4d4")), vec!["d4d4"]);
        // Taken dates: a1 day -1, b2 -2, c3 -3, d4 -4; e5 has none, so its import date (day 150).
        assert_eq!(search(&i, "date:2023-12-29..2023-12-30"), vec!["c3c3", "b2b2"]);
        assert_eq!(search(&i, "date:2024"), vec!["e5e5"]);
        assert_eq!(search(&i, "date:<2023-12-29"), vec!["d4d4"]);
        // Imported a1 day 30 … e5 day 150; "now" is day 200.
        assert_eq!(search(&i, "imported:<90d"), vec!["e5e5", "d4d4"]);
        assert_eq!(search(&i, "imported:>100d"), vec!["c3c3", "b2b2", "a1a1"]);
        assert_eq!(search(&i, "~kind:video ~animated:yes"), vec!["d4d4", "c3c3"]);
        assert_eq!(search(&i, "dupes:yes"), vec!["c3c3", "a1a1"]);
        assert_eq!(search(&i, "dupes:no"), vec!["e5e5", "d4d4", "b2b2"]);
    }

    #[test]
    fn orders() {
        let dir = tempfile::tempdir().unwrap();
        let i = SearchIndex::build(&collection(dir.path()));
        assert_eq!(search(&i, "order:filesize"), vec!["d4d4", "b2b2", "e5e5", "a1a1", "c3c3"]);
        assert_eq!(search(&i, "order:-filesize"), vec!["c3c3", "a1a1", "e5e5", "b2b2", "d4d4"]);
        assert_eq!(search(&i, "order:width"), vec!["e5e5", "a1a1", "c3c3", "b2b2", "d4d4"], "no width goes last");
        assert_eq!(search(&i, "order:-width"), vec!["b2b2", "c3c3", "a1a1", "e5e5", "d4d4"], "also when reversed");
        assert_eq!(search(&i, "order:date"), vec!["e5e5", "a1a1", "b2b2", "c3c3", "d4d4"]);
        assert_eq!(search(&i, "order:-imported"), vec!["a1a1", "b2b2", "c3c3", "d4d4", "e5e5"]);
        assert_eq!(search(&i, "order:name"), vec!["a1a1", "b2b2", "c3c3", "d4d4", "e5e5"], "case-insensitive");
        assert_eq!(search(&i, "order:path"), vec!["b2b2", "e5e5", "a1a1", "c3c3", "d4d4"]);
        assert_eq!(search(&i, "order:tagcount"), vec!["c3c3", "b2b2", "a1a1", "d4d4", "e5e5"]);
        let shuffled = |seed| {
            let mut q = Query::parse("order:random").unwrap();
            q.seed = seed;
            i.search_at(&q, day(0), &Links::default()).into_iter().map(str::to_string).collect::<Vec<_>>()
        };
        assert_eq!(shuffled(1), shuffled(1), "same seed, same order");
        assert!((2..20).any(|s| shuffled(s) != shuffled(1)), "seeds change the order");
    }
}
