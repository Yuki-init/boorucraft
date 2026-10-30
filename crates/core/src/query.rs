//! The booru search language.
//!
//! A query is a list of whitespace-separated terms:
//!
//! - `cat` — must have the tag; `-cat` — must not; `~cat ~dog` — at least one of the `~` terms.
//! - `cat*` — wildcard: any tag matching the pattern.
//! - Metatags: `rating:s,q`, `width:>1920`, `ratio:16:9`, `filesize:<2MB`, `type:png,gif`,
//!   `kind:video`, `animated:yes`, `date:2024-01..2024-06`, `imported:<7d`, `tagcount:0`,
//!   `path:"holiday 2023"`, `missing:yes`, `md5:…`, `hash:…`, `source:none`.
//! - `order:date`, `order:-size`, `order:random`: how results are sorted (one per query).
//!
//! Values may be quoted to include spaces. The parser reports errors with byte spans so a search
//! bar can underline them.

use std::ops::Range;

use chrono::{DateTime, Months, NaiveDate, TimeDelta, Utc};
use thiserror::Error;

use crate::rating::Rating;
use crate::tag::normalize_tag;

/// Longest query accepted, in bytes.
pub const MAX_QUERY_LEN: usize = 8192;
/// Most terms in one query.
pub const MAX_TERMS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Plain term: must match.
    Must,
    /// `-term`: must not match.
    Not,
    /// `~term`: at least one `~` term must match.
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagPattern {
    Exact(String),
    /// Contains `*`.
    Wildcard(String),
}

/// A comparison written before a value: `>`, `>=`, `<`, `<=` or none (`=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Numeric file properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumField {
    Width,
    Height,
    /// Width divided by height.
    Ratio,
    /// Width × height in millions.
    Mpixels,
    /// Bytes.
    Filesize,
    Tagcount,
}

/// Accepted values of a numeric metatag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumRange {
    pub min: Option<f64>,
    pub min_inclusive: bool,
    pub max: Option<f64>,
    pub max_inclusive: bool,
}

impl NumRange {
    pub fn contains(&self, v: f64) -> bool {
        let above = match self.min {
            None => true,
            Some(m) if self.min_inclusive => v >= m,
            Some(m) => v > m,
        };
        let below = match self.max {
            None => true,
            Some(m) if self.max_inclusive => v <= m,
            Some(m) => v < m,
        };
        above && below
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateField {
    /// When the picture was taken (EXIF), else the file's modification time.
    Taken,
    /// When BooruCraft first saw the file.
    Imported,
}

/// One date in a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateValue {
    /// A calendar year, month or day: `[start, end)`, in UTC.
    Period { start: DateTime<Utc>, end: DateTime<Utc> },
    /// An age such as `7d`, measured back from the time of the search.
    Ago(TimeDelta),
}

/// A date filter: `date:2024`, `date:>2023-05-01`, `date:2024-01..2024-06`, `imported:<7d`.
///
/// Ages compare by age, so `imported:<7d` means "less than 7 days old".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateSpec {
    pub cmp: Cmp,
    pub value: DateValue,
    /// The end of a `a..b` range (then `cmp` is `Eq`).
    pub to: Option<DateValue>,
}

impl DateSpec {
    /// The accepted time span `[from, until)` for a search made at `now`.
    pub fn resolve(&self, now: DateTime<Utc>) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
        let period = |v: &DateValue| match *v {
            DateValue::Period { start, end } => (start, end),
            DateValue::Ago(d) => {
                let t = now.checked_sub_signed(d).unwrap_or(DateTime::<Utc>::MIN_UTC);
                (t, t)
            }
        };
        if let Some(to) = &self.to {
            let (a, b) = (period(&self.value), period(to));
            return (Some(a.0.min(b.0)), Some(a.1.max(b.1)));
        }
        let (start, end) = period(&self.value);
        match (self.value, self.cmp) {
            (DateValue::Period { .. }, Cmp::Eq) => (Some(start), Some(end)),
            (DateValue::Period { .. }, Cmp::Lt) => (None, Some(start)),
            (DateValue::Period { .. }, Cmp::Le) => (None, Some(end)),
            (DateValue::Period { .. }, Cmp::Gt) => (Some(end), None),
            (DateValue::Period { .. }, Cmp::Ge) => (Some(start), None),
            // Younger than the age: after the instant.
            (DateValue::Ago(_), Cmp::Eq | Cmp::Lt | Cmp::Le) => (Some(start), None),
            (DateValue::Ago(_), Cmp::Gt | Cmp::Ge) => (None, Some(start)),
        }
    }
}

/// Matching text such as a path or a source URL, lowercased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextMatch {
    Contains(String),
    /// Contains `*`; must match the whole text.
    Glob(String),
}

impl TextMatch {
    /// `text` must already be lowercase.
    pub fn matches(&self, text: &str) -> bool {
        match self {
            TextMatch::Contains(s) => text.contains(s.as_str()),
            TextMatch::Glob(p) => crate::glob_match(p, text),
        }
    }
}

/// What `parent:` matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParentMatch {
    /// `parent:none`: files without a parent.
    None,
    /// `parent:any`: files with a parent.
    Any,
    /// `parent:3fa9`: the file whose SHA-256 starts with this, and its children.
    Of(String),
}

/// What `pool:` matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolMatch {
    /// `pool:none`: files in no pool.
    None,
    /// `pool:any`: files in some pool.
    Any,
    Name(String),
    /// `pool:trip_*`.
    Glob(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TermKind {
    Tag(TagPattern),
    /// Any of these ratings.
    Rating(Vec<Rating>),
    Number(NumField, NumRange),
    Date(DateField, DateSpec),
    /// Any of these formats (normalised: `jpeg`, `tiff`…).
    Format(Vec<String>),
    /// Any of `image`, `video`, `other`.
    Kind(Vec<String>),
    Animated(bool),
    Missing(bool),
    /// Has (or hasn't) a near-duplicate in the open collections.
    Dupes(bool),
    /// Any of the file's paths.
    Path(TextMatch),
    Md5(String),
    /// Prefix of the SHA-256.
    Hash(String),
    /// `None`: the file has no source.
    Source(Option<TextMatch>),
    Parent(ParentMatch),
    /// Has (or hasn't) children.
    Child(bool),
    Pool(PoolMatch),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKey {
    /// When BooruCraft first saw the file (the default order, newest first).
    Imported,
    /// When the picture was taken.
    Date,
    Filesize,
    Width,
    Height,
    Mpixels,
    Ratio,
    Tagcount,
    /// File name.
    Name,
    Path,
    Random,
    /// Position in the pool named by the search's `pool:` term.
    Pool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Order {
    pub key: OrderKey,
    /// Largest, newest or last first.
    pub descending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub mode: Mode,
    pub kind: TermKind,
    /// Byte range of the term in the query text.
    pub span: Range<usize>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub terms: Vec<Term>,
    /// `order:`; `None` sorts newest imported first.
    pub order: Option<Order>,
    /// Shuffles `order:random`; the caller picks it (the same seed gives the same order).
    pub seed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct QueryError {
    pub message: String,
    /// Byte range to underline.
    pub span: Range<usize>,
}

fn err(message: impl Into<String>, span: Range<usize>) -> QueryError {
    QueryError { message: message.into(), span }
}

/// Splits on whitespace outside double quotes, keeping byte offsets.
fn tokens(input: &str) -> Result<Vec<(&str, Range<usize>)>, QueryError> {
    let mut start = None;
    let mut quote: Option<usize> = None;
    let mut out = Vec::new();
    for (i, c) in input.char_indices() {
        if c == '"' {
            quote = if quote.is_some() { None } else { Some(i) };
        }
        match (c.is_whitespace() && quote.is_none(), start) {
            (true, Some(s)) => {
                out.push((&input[s..i], s..i));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(q) = quote {
        return Err(err("this quote isn't closed", q..input.len()));
    }
    if let Some(s) = start {
        out.push((&input[s..], s..input.len()));
    }
    Ok(out)
}

impl Query {
    pub fn parse(input: &str) -> Result<Query, QueryError> {
        if input.len() > MAX_QUERY_LEN {
            return Err(err(format!("query is longer than {MAX_QUERY_LEN} bytes"), 0..input.len()));
        }
        let mut query = Query::default();
        let mut order_span: Option<Range<usize>> = None;
        for (token, span) in tokens(input)? {
            if query.terms.len() >= MAX_TERMS {
                return Err(err(format!("more than {MAX_TERMS} terms"), span));
            }
            match parse_term(token, span.clone())? {
                Parsed::Term(t) => query.terms.push(t),
                Parsed::Order(o) => {
                    if order_span.is_some() {
                        return Err(err("only one order: per search", span));
                    }
                    order_span = Some(span);
                    query.order = Some(o);
                }
            }
        }
        Ok(query)
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// The pool whose order the results follow: the first required `pool:name` term, when the
    /// search has no `order:` or has `order:pool`.
    pub fn pool_order(&self) -> Option<&str> {
        if self.order.is_some_and(|o| o.key != OrderKey::Pool) {
            return None;
        }
        self.terms.iter().find_map(|t| match (&t.mode, &t.kind) {
            (Mode::Must, TermKind::Pool(PoolMatch::Name(n))) => Some(n.as_str()),
            _ => None,
        })
    }
}

enum Parsed {
    Term(Term),
    Order(Order),
}

fn parse_term(token: &str, span: Range<usize>) -> Result<Parsed, QueryError> {
    let (mode, body) = if let Some(rest) = token.strip_prefix('-') {
        (Mode::Not, rest)
    } else if let Some(rest) = token.strip_prefix('~') {
        (Mode::Any, rest)
    } else {
        (Mode::Must, token)
    };
    if body.is_empty() {
        return Err(err(format!("expected a tag after `{token}`"), span));
    }
    if let Some((name, value)) = body.split_once(':') {
        let name = name.to_ascii_lowercase();
        if crate::METATAGS.contains(&name.as_str()) {
            let e = |m: String| err(m, span.clone());
            if name == "order" {
                if mode != Mode::Must {
                    return Err(e("order: can't take `-` or `~`; write order:-date for the reverse".into()));
                }
                return parse_order(value).map(Parsed::Order).map_err(e);
            }
            let kind = parse_metatag(&name, value).map_err(e)?;
            return Ok(Parsed::Term(Term { mode, kind, span }));
        }
    }
    let lower = body.to_lowercase();
    let pattern = if lower.contains('*') {
        if lower.starts_with('-') || lower.starts_with('~') {
            return Err(err(format!("`{body}`: only one of `-` and `~` can start a term"), span));
        }
        TagPattern::Wildcard(lower)
    } else {
        TagPattern::Exact(normalize_tag(&lower).map_err(|e| err(e.to_string(), span.clone()))?)
    };
    Ok(Parsed::Term(Term { mode, kind: TermKind::Tag(pattern), span }))
}

/// Removes surrounding double quotes.
fn unquote(value: &str) -> &str {
    value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value)
}

fn parse_metatag(name: &str, value: &str) -> Result<TermKind, String> {
    let value = unquote(value);
    if value.is_empty() {
        return Err(format!("`{name}:` needs a value, e.g. {}", example(name)));
    }
    let lower = value.to_lowercase();
    match name {
        "rating" => {
            let mut ratings = Vec::new();
            for part in lower.split(',') {
                let r = Rating::parse(part).ok_or_else(|| format!("unknown rating `{part}`; use s, q or e"))?;
                if !ratings.contains(&r) {
                    ratings.push(r);
                }
            }
            Ok(TermKind::Rating(ratings))
        }
        "width" | "height" | "tagcount" => {
            let field = match name {
                "width" => NumField::Width,
                "height" => NumField::Height,
                _ => NumField::Tagcount,
            };
            let range = num_range(&lower, |s| s.parse::<u32>().ok().map(f64::from), 0.0).ok_or_else(|| bad(name))?;
            Ok(TermKind::Number(field, range))
        }
        "mpixels" => Ok(TermKind::Number(NumField::Mpixels, num_range(&lower, parse_float, 0.05).ok_or_else(|| bad(name))?)),
        "ratio" => Ok(TermKind::Number(NumField::Ratio, num_range(&lower, parse_ratio, 0.01).ok_or_else(|| bad(name))?)),
        "filesize" => Ok(TermKind::Number(NumField::Filesize, num_range(&lower, parse_size, 0.05).ok_or_else(|| bad(name))?)),
        "date" | "imported" => {
            let field = if name == "date" { DateField::Taken } else { DateField::Imported };
            Ok(TermKind::Date(field, parse_date_spec(&lower).ok_or_else(|| bad(name))?))
        }
        "type" => {
            let mut formats = Vec::new();
            for part in lower.split(',') {
                if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    return Err(format!("`{part}` isn't a file type; try {}", example(name)));
                }
                let f = normalize_format(part).to_string();
                if !formats.contains(&f) {
                    formats.push(f);
                }
            }
            Ok(TermKind::Format(formats))
        }
        "kind" => {
            let mut kinds = Vec::new();
            for part in lower.split(',') {
                let k = match part {
                    "image" | "picture" | "pic" => "image",
                    "video" => "video",
                    "other" => "other",
                    _ => return Err(format!("unknown kind `{part}`; use image, video or other")),
                };
                if !kinds.iter().any(|x| x == k) {
                    kinds.push(k.to_string());
                }
            }
            Ok(TermKind::Kind(kinds))
        }
        "animated" => Ok(TermKind::Animated(parse_bool(&lower).ok_or_else(|| bad(name))?)),
        "missing" => Ok(TermKind::Missing(parse_bool(&lower).ok_or_else(|| bad(name))?)),
        "dupes" => Ok(TermKind::Dupes(parse_bool(&lower).ok_or_else(|| bad(name))?)),
        "path" => Ok(TermKind::Path(text_match(&lower))),
        "source" if lower == "none" => Ok(TermKind::Source(None)),
        "source" => Ok(TermKind::Source(Some(text_match(&lower)))),
        "parent" => Ok(TermKind::Parent(match lower.as_str() {
            "none" => ParentMatch::None,
            "any" => ParentMatch::Any,
            h if (4..=64).contains(&h.len()) && h.bytes().all(|b| b.is_ascii_hexdigit()) => ParentMatch::Of(h.to_string()),
            _ => return Err(bad(name)),
        })),
        "pool" => Ok(TermKind::Pool(match lower.as_str() {
            "none" => PoolMatch::None,
            "any" => PoolMatch::Any,
            p if p.contains('*') => PoolMatch::Glob(p.to_string()),
            p => PoolMatch::Name(normalize_tag(p).map_err(|e| e.to_string())?),
        })),
        "child" => Ok(TermKind::Child(match lower.as_str() {
            "any" => true,
            "none" => false,
            v => parse_bool(v).ok_or_else(|| bad(name))?,
        })),
        "md5" => {
            if lower.len() == 32 && lower.bytes().all(|b| b.is_ascii_hexdigit()) {
                Ok(TermKind::Md5(lower))
            } else {
                Err("md5: needs 32 hex digits".into())
            }
        }
        "hash" => {
            if (4..=64).contains(&lower.len()) && lower.bytes().all(|b| b.is_ascii_hexdigit()) {
                Ok(TermKind::Hash(lower))
            } else {
                Err("hash: needs 4 to 64 hex digits of the SHA-256".into())
            }
        }
        _ => Err(format!("`{name}:` searches aren't supported")),
    }
}

fn bad(name: &str) -> String {
    format!("can't read this `{name}:` value; try {}", example(name))
}

/// An example for each metatag, shown in error messages and autocomplete.
pub fn example(name: &str) -> &'static str {
    METATAG_HELP.iter().find(|(n, _)| *n == name).map_or("", |(_, e)| e)
}

/// Metatags offered by autocomplete, with an example each.
pub const METATAG_HELP: &[(&str, &str)] = &[
    ("rating", "rating:s or rating:q,e"),
    ("width", "width:>1920"),
    ("height", "height:<=1080"),
    ("ratio", "ratio:16:9"),
    ("mpixels", "mpixels:>2"),
    ("filesize", "filesize:<2MB"),
    ("type", "type:png,gif"),
    ("kind", "kind:video"),
    ("animated", "animated:yes"),
    ("date", "date:2024-01..2024-06"),
    ("imported", "imported:<7d"),
    ("tagcount", "tagcount:0"),
    ("path", "path:\"holiday 2023\""),
    ("missing", "missing:yes"),
    ("dupes", "dupes:yes"),
    ("parent", "parent:3fa9 (it and its children) or parent:none"),
    ("child", "child:yes (has children)"),
    ("pool", "pool:my_comic (in its order) or pool:none"),
    ("source", "source:none or source:*pixiv*"),
    ("md5", "md5:<32 hex digits>"),
    ("hash", "hash:3fa9"),
    ("order", "order:date or order:-filesize"),
];

/// Values autocomplete offers after `name:`.
pub fn metatag_values(name: &str) -> &'static [&'static str] {
    match name {
        "rating" => &["s", "q", "e", "s,q", "q,e"],
        "kind" => &["image", "video", "other"],
        "animated" | "missing" | "dupes" | "child" => &["yes", "no"],
        "parent" | "pool" => &["any", "none"],
        "type" => &["jpeg", "png", "gif", "webp", "jxl", "avif", "heic", "mp4", "webm", "mkv"],
        "ratio" => &["16:9", "4:3", "3:2", "1:1", "9:16", ">1", "<1"],
        "tagcount" => &["0", ">10"],
        "imported" | "date" => &["<1d", "<7d", "<30d", ">1y"],
        "source" => &["none"],
        "order" => &["imported", "date", "-date", "filesize", "width", "height", "mpixels", "ratio", "tagcount", "name", "path", "random", "pool"],
        _ => &[],
    }
}

/// Common spellings of a format name → the name the scanner stores.
pub fn normalize_format(f: &str) -> &str {
    match f {
        "jpg" | "jpe" | "jfif" => "jpeg",
        "tif" => "tiff",
        "htm" => "html",
        other => other,
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "yes" | "true" | "1" => Some(true),
        "no" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn text_match(v: &str) -> TextMatch {
    if v.contains('*') { TextMatch::Glob(v.to_string()) } else { TextMatch::Contains(v.to_string()) }
}

/// Strips a leading comparison.
fn split_cmp(v: &str) -> (Cmp, &str) {
    for (p, c) in [(">=", Cmp::Ge), ("<=", Cmp::Le), (">", Cmp::Gt), ("<", Cmp::Lt), ("=", Cmp::Eq)] {
        if let Some(rest) = v.strip_prefix(p) {
            return (c, rest);
        }
    }
    (Cmp::Eq, v)
}

/// `>5`, `5`, `5..10`, `..10`, `5..`. `tolerance` makes a plain value match ±that fraction.
fn num_range(v: &str, parse: impl Fn(&str) -> Option<f64>, tolerance: f64) -> Option<NumRange> {
    let ok = |x: f64| x.is_finite() && x >= 0.0;
    if let Some((a, b)) = v.split_once("..") {
        let min = if a.is_empty() { None } else { Some(parse(a).filter(|x| ok(*x))?) };
        let max = if b.is_empty() { None } else { Some(parse(b).filter(|x| ok(*x))?) };
        if min.is_none() && max.is_none() {
            return None;
        }
        return Some(NumRange { min, min_inclusive: true, max, max_inclusive: true });
    }
    let (cmp, rest) = split_cmp(v);
    let x = parse(rest).filter(|x| ok(*x))?;
    let r = |min, min_inclusive, max, max_inclusive| NumRange { min, min_inclusive, max, max_inclusive };
    Some(match cmp {
        Cmp::Eq => r(Some(x * (1.0 - tolerance)), true, Some(x * (1.0 + tolerance)), true),
        Cmp::Lt => r(None, false, Some(x), false),
        Cmp::Le => r(None, false, Some(x), true),
        Cmp::Gt => r(Some(x), false, None, false),
        Cmp::Ge => r(Some(x), true, None, false),
    })
}

fn parse_float(s: &str) -> Option<f64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    s.parse().ok()
}

/// `16:9`, `16/9` or `1.78`.
fn parse_ratio(s: &str) -> Option<f64> {
    match s.split_once([':', '/']) {
        Some((w, h)) => {
            let (w, h) = (parse_float(w)?, parse_float(h)?);
            (h > 0.0).then(|| w / h)
        }
        None => parse_float(s),
    }
}

/// `2mb`, `500kb`, `1.5g`, `1024` (bytes). Units are powers of 1024.
fn parse_size(s: &str) -> Option<f64> {
    let digits = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(digits);
    let mul = match unit {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some(parse_float(num)? * mul)
}

fn parse_date_spec(v: &str) -> Option<DateSpec> {
    if let Some((a, b)) = v.split_once("..") {
        // Open ends become comparisons.
        return match (a.is_empty(), b.is_empty()) {
            (false, false) => Some(DateSpec { cmp: Cmp::Eq, value: parse_date(a)?, to: Some(parse_date(b)?) }),
            (false, true) => Some(DateSpec { cmp: Cmp::Ge, value: parse_date(a)?, to: None }),
            (true, false) => Some(DateSpec { cmp: Cmp::Le, value: parse_date(b)?, to: None }),
            (true, true) => None,
        };
    }
    let (cmp, rest) = split_cmp(v);
    Some(DateSpec { cmp, value: parse_date(rest)?, to: None })
}

/// `2024`, `2024-01`, `2024-01-15`, or an age: `12h`, `7d`, `2w`, `3mo`, `1y`.
fn parse_date(s: &str) -> Option<DateValue> {
    if let Some(age) = parse_age(s) {
        return Some(DateValue::Ago(age));
    }
    let parts: Vec<&str> = s.split('-').collect();
    if parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let num = |i: usize| parts.get(i).and_then(|p| p.parse::<u32>().ok());
    let year = i32::try_from(num(0)?).ok().filter(|y| (1000..=9999).contains(y))?;
    let (start, end) = match parts.len() {
        1 => {
            let start = NaiveDate::from_ymd_opt(year, 1, 1)?;
            (start, start.checked_add_months(Months::new(12))?)
        }
        2 => {
            let start = NaiveDate::from_ymd_opt(year, num(1)?, 1)?;
            (start, start.checked_add_months(Months::new(1))?)
        }
        3 => {
            let start = NaiveDate::from_ymd_opt(year, num(1)?, num(2)?)?;
            (start, start.succ_opt()?)
        }
        _ => return None,
    };
    let utc = |d: NaiveDate| d.and_hms_opt(0, 0, 0).map(|t| t.and_utc());
    Some(DateValue::Period { start: utc(start)?, end: utc(end)? })
}

fn parse_age(s: &str) -> Option<TimeDelta> {
    let digits = s.find(|c: char| !c.is_ascii_digit())?;
    let (num, unit) = s.split_at(digits);
    let n: i64 = num.parse().ok().filter(|n| *n <= 100_000)?;
    let hours = match unit {
        "h" => n,
        "d" => n * 24,
        "w" => n * 24 * 7,
        "mo" => n * 24 * 30,
        "y" => n * 24 * 365,
        _ => return None,
    };
    if hours > 24 * 365 * 10_000 {
        return None;
    }
    TimeDelta::try_hours(hours)
}

/// `date`, `-date`, `date_asc`, `date_desc`…
fn parse_order(v: &str) -> Result<Order, String> {
    let v = unquote(v).to_lowercase();
    let (reverse, name) = match v.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, v.as_str()),
    };
    let (name, explicit) = if let Some(n) = name.strip_suffix("_asc") {
        (n, Some(false))
    } else if let Some(n) = name.strip_suffix("_desc") {
        (n, Some(true))
    } else {
        (name, None)
    };
    let key = match name {
        "imported" | "id" => OrderKey::Imported,
        "date" | "taken" => OrderKey::Date,
        "filesize" | "size" => OrderKey::Filesize,
        "width" => OrderKey::Width,
        "height" => OrderKey::Height,
        "mpixels" => OrderKey::Mpixels,
        "ratio" => OrderKey::Ratio,
        "tagcount" => OrderKey::Tagcount,
        "name" => OrderKey::Name,
        "path" => OrderKey::Path,
        "random" => OrderKey::Random,
        "pool" => OrderKey::Pool,
        _ => return Err(format!("unknown order `{name}`; try {}", metatag_values("order").join(", "))),
    };
    // Names, paths and pool pages read A→Z / first to last; everything else largest or newest
    // first.
    let natural_desc = !matches!(key, OrderKey::Name | OrderKey::Path | OrderKey::Pool);
    let descending = explicit.unwrap_or(natural_desc) != reverse;
    Ok(Order { key, descending })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn kinds(q: &str) -> Vec<(Mode, TermKind)> {
        Query::parse(q).unwrap().terms.into_iter().map(|t| (t.mode, t.kind)).collect()
    }

    fn one(q: &str) -> TermKind {
        let mut k = kinds(q);
        assert_eq!(k.len(), 1, "{q}");
        k.remove(0).1
    }

    fn tag(s: &str) -> TermKind {
        TermKind::Tag(TagPattern::Exact(s.into()))
    }

    fn range(q: &str) -> NumRange {
        match one(q) {
            TermKind::Number(_, r) => r,
            other => panic!("{q}: {other:?}"),
        }
    }

    fn utc(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn plain_negated_and_or_terms() {
        assert_eq!(
            kinds("  Cat -dog ~sky ~SEA artist:Foo  "),
            vec![(Mode::Must, tag("cat")), (Mode::Not, tag("dog")), (Mode::Any, tag("sky")), (Mode::Any, tag("sea")), (Mode::Must, tag("artist:foo")),]
        );
        assert!(Query::parse("   ").unwrap().is_empty());
        // Booru face tags start with `:`; only a metatag name before the `:` makes a filter.
        assert_eq!(kinds(":D -:3 ~:t"), vec![(Mode::Must, tag(":d")), (Mode::Not, tag(":3")), (Mode::Any, tag(":t"))]);
        assert!(Query::parse("cat:").is_err());
    }

    #[test]
    fn wildcards_and_ratings() {
        assert_eq!(
            kinds("cat* -rating:e"),
            vec![(Mode::Must, TermKind::Tag(TagPattern::Wildcard("cat*".into()))), (Mode::Not, TermKind::Rating(vec![Rating::Explicit]))]
        );
        assert_eq!(kinds("rating:q,E,q"), vec![(Mode::Must, TermKind::Rating(vec![Rating::Questionable, Rating::Explicit]))]);
    }

    #[test]
    fn numbers() {
        let r = range("width:>1920");
        assert!(!r.contains(1920.0) && r.contains(1921.0));
        let r = range("height:<=1080");
        assert!(r.contains(1080.0) && !r.contains(1081.0));
        let r = range("width:1000..2000");
        assert!(r.contains(1000.0) && r.contains(2000.0) && !r.contains(2001.0));
        let r = range("width:..500");
        assert!(r.contains(0.0) && !r.contains(501.0));
        let r = range("tagcount:0");
        assert!(r.contains(0.0) && !r.contains(1.0));
        let r = range("ratio:16:9");
        assert!(r.contains(1920.0 / 1080.0) && r.contains(1366.0 / 768.0) && !r.contains(4.0 / 3.0));
        let r = range("ratio:>1");
        assert!(r.contains(1.5) && !r.contains(1.0));
        let r = range("filesize:<2MB");
        assert!(r.contains(1_000_000.0) && !r.contains(3.0 * 1024.0 * 1024.0));
        let r = range("filesize:500kb");
        assert!(r.contains(500.0 * 1024.0 + 1000.0) && !r.contains(400.0 * 1024.0));
        let r = range("mpixels:>=2.5");
        assert!(r.contains(2.5) && !r.contains(2.4));
        for bad in ["width:big", "width:>", "width:1.5", "width:..", "ratio:16:0", "filesize:2xb", "mpixels:-1", "width:-5"] {
            assert!(Query::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn dates() {
        let now = utc(2026, 10, 29);
        let spec = |q: &str| match one(q) {
            TermKind::Date(_, s) => s.resolve(now),
            other => panic!("{q}: {other:?}"),
        };
        assert_eq!(spec("date:2024"), (Some(utc(2024, 1, 1)), Some(utc(2025, 1, 1))));
        assert_eq!(spec("date:2024-02"), (Some(utc(2024, 2, 1)), Some(utc(2024, 3, 1))));
        assert_eq!(spec("date:2024-12-31"), (Some(utc(2024, 12, 31)), Some(utc(2025, 1, 1))));
        assert_eq!(spec("date:2024-01..2024-06"), (Some(utc(2024, 1, 1)), Some(utc(2024, 7, 1))));
        assert_eq!(spec("date:2024-06..2024-01"), (Some(utc(2024, 1, 1)), Some(utc(2024, 7, 1))), "either way round");
        assert_eq!(spec("date:>2023-05-01"), (Some(utc(2023, 5, 2)), None));
        assert_eq!(spec("date:>=2023-05-01"), (Some(utc(2023, 5, 1)), None));
        assert_eq!(spec("date:<2023"), (None, Some(utc(2023, 1, 1))));
        assert_eq!(spec("date:2020.."), (Some(utc(2020, 1, 1)), None));
        assert_eq!(spec("imported:<7d"), (Some(utc(2026, 10, 22)), None), "less than 7 days old");
        assert_eq!(spec("imported:>2w"), (None, Some(utc(2026, 10, 15))), "older than 2 weeks");
        assert_eq!(spec("imported:7d..30d"), (Some(utc(2026, 9, 29)), Some(utc(2026, 10, 22))));
        assert!(matches!(one("date:1y"), TermKind::Date(DateField::Taken, _)));
        for bad in ["date:2024-13", "date:2024-02-30", "date:yesterday", "date:24", "imported:5m", "date:..", "date:2024-01-01-01"] {
            assert!(Query::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn file_metatags() {
        assert_eq!(one("type:JPG,jpeg,png"), TermKind::Format(vec!["jpeg".into(), "png".into()]));
        assert_eq!(one("kind:video,image"), TermKind::Kind(vec!["video".into(), "image".into()]));
        assert_eq!(one("animated:yes"), TermKind::Animated(true));
        assert_eq!(one("missing:no"), TermKind::Missing(false));
        assert_eq!(one("dupes:yes"), TermKind::Dupes(true));
        assert_eq!(one("parent:none"), TermKind::Parent(ParentMatch::None));
        assert_eq!(one("parent:3FA9"), TermKind::Parent(ParentMatch::Of("3fa9".into())));
        assert_eq!(one("child:any"), TermKind::Child(true));
        assert_eq!(one("pool:My_Comic"), TermKind::Pool(PoolMatch::Name("my_comic".into())));
        assert_eq!(one("pool:trip*"), TermKind::Pool(PoolMatch::Glob("trip*".into())));
        assert_eq!(Query::parse("cat pool:comic").unwrap().pool_order(), Some("comic"));
        assert_eq!(Query::parse("pool:comic order:-pool").unwrap().pool_order(), Some("comic"));
        assert_eq!(Query::parse("pool:comic order:date").unwrap().pool_order(), None);
        assert_eq!(Query::parse("-pool:comic").unwrap().pool_order(), None);
        assert!(Query::parse("parent:xyz").is_err() && Query::parse("child:maybe").is_err());
        assert_eq!(one("path:\"Holiday 2023\""), TermKind::Path(TextMatch::Contains("holiday 2023".into())));
        assert_eq!(one("path:*/2023/*"), TermKind::Path(TextMatch::Glob("*/2023/*".into())));
        assert_eq!(one("source:none"), TermKind::Source(None));
        assert_eq!(one("source:*pixiv*"), TermKind::Source(Some(TextMatch::Glob("*pixiv*".into()))));
        assert_eq!(one("hash:3FA9"), TermKind::Hash("3fa9".into()));
        assert!(matches!(one("md5:0123456789abcdef0123456789ABCDEF"), TermKind::Md5(_)));
        for bad in ["kind:sound", "animated:maybe", "md5:abc", "hash:xyz1", "type:p.ng", "dupes:maybe", "path:", "path:\"\""] {
            assert!(Query::parse(bad).is_err(), "{bad}");
        }
        assert!(TextMatch::Glob("*/2023/*".into()).matches("trips/2023/a.jpg"));
        assert!(!TextMatch::Contains("x".into()).matches("abc"));
    }

    #[test]
    fn quotes() {
        let q = Query::parse("cat path:\"my holiday/2023\" ~dog").unwrap();
        assert_eq!(q.terms.len(), 3);
        assert_eq!(q.terms[1].span, 4..26);
        let e = Query::parse("cat path:\"open").unwrap_err();
        assert_eq!(e.span, 9..14);
        assert_eq!(e.message, "this quote isn't closed");
    }

    #[test]
    fn order() {
        let o = |q: &str| Query::parse(q).unwrap().order;
        assert_eq!(o("cat"), None);
        assert_eq!(o("order:date cat"), Some(Order { key: OrderKey::Date, descending: true }));
        assert_eq!(o("order:-date"), Some(Order { key: OrderKey::Date, descending: false }));
        assert_eq!(o("order:date_asc"), Some(Order { key: OrderKey::Date, descending: false }));
        assert_eq!(o("order:size"), Some(Order { key: OrderKey::Filesize, descending: true }));
        assert_eq!(o("order:name"), Some(Order { key: OrderKey::Name, descending: false }));
        assert_eq!(o("order:-name"), Some(Order { key: OrderKey::Name, descending: true }));
        assert_eq!(o("order:random").map(|o| o.key), Some(OrderKey::Random));
        assert!(Query::parse("cat order:date").unwrap().terms.len() == 1, "order isn't a filter");
        let e = Query::parse("order:date order:size").unwrap_err();
        assert_eq!(e.span, 11..21);
        for bad in ["order:score", "-order:date", "~order:date", "order:"] {
            assert!(Query::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn spans_point_at_the_bad_term() {
        let e = Query::parse("cat rating:x").unwrap_err();
        assert_eq!(e.span, 4..12);
        let e = Query::parse("cat - dog").unwrap_err();
        assert_eq!(e.span, 4..5);
        let e = Query::parse("ok width:>big").unwrap_err();
        assert_eq!(e.span, 3..13);
        assert!(e.message.contains("width:>1920"), "errors give an example: {}", e.message);
        let e = Query::parse("é --x").unwrap_err();
        assert_eq!(e.span, 3..6);
    }

    #[test]
    fn limits() {
        assert!(Query::parse(&"a ".repeat(MAX_TERMS + 1)).is_err());
        assert!(Query::parse(&"a".repeat(MAX_QUERY_LEN + 1)).is_err());
        assert!(Query::parse("imported:<99999999999d").is_err());
        assert!(Query::parse("date:>100000y").is_err());
    }

    #[test]
    fn every_metatag_has_help() {
        for name in crate::METATAGS {
            assert!(!example(name).is_empty(), "{name}");
        }
    }
}
