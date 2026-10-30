//! Importing tags, ratings and sources from boorus by MD5.
//!
//! 1. [`lookup`] asks the sites in the settings' order for a picture's MD5 and stops at the first
//!    that has it.
//! 2. [`plan`] works out what that post would change for one file without changing anything, so
//!    a dry run or the UI can show it first.
//! 3. [`apply`] makes the change. It only adds: tags already on the file stay, and the rating and
//!    source are filled in only where the file has none.
//!
//! [`BooruSettings`] (sites, accounts, choices) live in `booru.json` in the config folder,
//! readable only by the user because it holds API keys. [`Lookups`] remembers what each site
//! answered for an MD5 (`booru-lookups.json` in the cache folder), so the next import skips
//! pictures it already asked about.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use boorucraft_booru::{BooruError, Client, Credentials, Post, Site};
use boorucraft_core::Rating;
use boorucraft_store::{Collection, FileEntry, StoreError, TagList, paths, safefile};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Largest settings or lookups file read.
const MAX_FILE: u64 = 64 << 20;
/// Lookups older than this are asked again (posts get added and retagged).
pub const LOOKUP_TTL_DAYS: i64 = 90;

/// An account on a site (Gelbooru: user id and API key; Danbooru: login and API key).
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub user: String,
    pub api_key: String,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account").field("user", &self.user).field("api_key", &"…").finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BooruSettings {
    /// Site ids to ask, in order; the first that has the picture wins.
    pub sites: Vec<String>,
    /// Accounts by site id.
    pub accounts: BTreeMap<String, Account>,
    /// Add tags the tag list doesn't have yet to it (otherwise they are left out).
    pub create_tags: bool,
    /// Set the rating of files that have none.
    pub fill_rating: bool,
    /// Set the source of files that have none: the post's source, else the post itself.
    pub fill_source: bool,
}

impl Default for BooruSettings {
    fn default() -> Self {
        BooruSettings {
            sites: vec![Site::Danbooru.id().into(), Site::Gelbooru.id().into(), Site::Safebooru.id().into()],
            accounts: BTreeMap::new(),
            create_tags: true,
            fill_rating: true,
            fill_source: true,
        }
    }
}

impl BooruSettings {
    /// `booru.json` in the config folder.
    pub fn path() -> PathBuf {
        paths::config_dir().join("booru.json")
    }

    /// The settings at `path`, or the defaults if there is no file yet.
    pub fn load(path: &Path) -> Result<BooruSettings, StoreError> {
        match safefile::read_capped(path, MAX_FILE)? {
            None => Ok(BooruSettings::default()),
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| StoreError::Invalid(format!("{}: {e}", path.display()))),
        }
    }

    /// Saves atomically, readable and writable only by the user (it holds API keys).
    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        let mut json = serde_json::to_vec_pretty(self).map_err(|e| StoreError::Invalid(e.to_string()))?;
        json.push(b'\n');
        write_private(path, &json)
    }

    /// The sites to ask, in order, leaving out unknown ids and sites that need an account
    /// nobody set up.
    pub fn active_sites(&self) -> Vec<Site> {
        let mut out = Vec::new();
        for site in self.sites.iter().filter_map(|s| Site::parse(s)) {
            if !out.contains(&site) && (!site.needs_key() || self.credentials(site).is_some()) {
                out.push(site);
            }
        }
        out
    }

    pub fn credentials(&self, site: Site) -> Option<Credentials> {
        let a = self.accounts.get(site.id())?;
        let c = Credentials { user: a.user.clone(), api_key: a.api_key.clone() };
        (!c.is_empty()).then_some(c)
    }
}

/// Writes `bytes` to `path` atomically with owner-only permissions on Unix.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
    }
    let tmp = safefile::sibling(path, ".tmp");
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(StoreError::io(path, e));
    }
    Ok(())
}

/// What one site answered for one MD5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The post id, or none if the site had no post.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<u64>,
    pub at: DateTime<Utc>,
    /// The post's tags were imported (not just looked at in a dry run).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub imported: bool,
}

/// Every site's answer for every MD5 asked about. A cache: deleting it only means asking again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lookups {
    pub answers: BTreeMap<String, BTreeMap<String, Answer>>,
}

impl Lookups {
    /// `booru-lookups.json` in the cache folder.
    pub fn path() -> PathBuf {
        paths::cache_dir().join("booru-lookups.json")
    }

    /// The lookups at `path`; empty if the file is missing or unreadable (it is only a cache).
    pub fn load(path: &Path) -> Lookups {
        safefile::read_capped(path, MAX_FILE).ok().flatten().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
        }
        let json = serde_json::to_vec(self).map_err(|e| StoreError::Invalid(e.to_string()))?;
        safefile::write_atomic(path, &json, false).map(|_| ())
    }

    /// Whether an import can skip `md5`: within [`LOOKUP_TTL_DAYS`], the first of `sites` that
    /// had a post was imported from, or none of them had one.
    pub fn can_skip(&self, md5: &str, sites: &[Site], now: DateTime<Utc>) -> bool {
        let Some(answers) = self.answers.get(md5) else { return false };
        for site in sites {
            match answers.get(site.id()).filter(|a| now - a.at < Duration::days(LOOKUP_TTL_DAYS)) {
                Some(a) if a.post.is_some() => return a.imported,
                Some(_) => {}
                None => return false,
            }
        }
        !sites.is_empty()
    }

    pub fn record(&mut self, md5: &str, site: Site, post: Option<u64>, now: DateTime<Utc>) {
        self.answers.entry(md5.to_string()).or_default().insert(site.id().to_string(), Answer { post, at: now, imported: false });
    }

    /// Notes that the post `site` found for `md5` was imported.
    pub fn mark_imported(&mut self, md5: &str, site: Site) {
        if let Some(a) = self.answers.get_mut(md5).and_then(|a| a.get_mut(site.id())) {
            a.imported = true;
        }
    }
}

/// The result of asking the sites about one picture.
#[derive(Debug, Default)]
pub struct Found {
    pub post: Option<Post>,
    /// Sites that couldn't answer (network errors, refusals), in the order asked.
    pub errors: Vec<BooruError>,
}

/// Asks `sites` in order for `md5`, stopping at the first that has it, and records each answer.
pub fn lookup(client: &mut Client, settings: &BooruSettings, sites: &[Site], md5: &str, lookups: &mut Lookups) -> Found {
    let mut found = Found::default();
    for &site in sites {
        match client.lookup(site, md5, settings.credentials(site).as_ref()) {
            Ok(post) => {
                lookups.record(md5, site, post.as_ref().map(|p| p.id), Utc::now());
                if post.is_some() {
                    found.post = post;
                    break;
                }
            }
            Err(e) => found.errors.push(e),
        }
    }
    found
}

/// What importing a post would change for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub site: Site,
    pub post_id: u64,
    pub post_url: String,
    /// Tags to add that the tag list has (as the list names them: aliases resolved).
    pub add: Vec<String>,
    /// Tags to add that are new to the tag list (created first).
    pub create: Vec<String>,
    /// Tags left out: new to the tag list while creating tags is off, or not valid names.
    pub left_out: Vec<String>,
    pub rating: Option<Rating>,
    pub source: Option<String>,
}

impl Change {
    /// Whether applying it would change nothing.
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.create.is_empty() && self.rating.is_none() && self.source.is_none()
    }

    /// How many tags it adds.
    pub fn tag_count(&self) -> usize {
        self.add.len() + self.create.len()
    }
}

/// Works out what importing `post` changes for `entry`, without changing anything.
pub fn plan(post: &Post, entry: &FileEntry, tags: &TagList, settings: &BooruSettings) -> Change {
    let mut add = Vec::new();
    let mut create = Vec::new();
    let mut left_out = post.skipped_tags.clone();
    for t in &post.tags {
        match tags.resolve(t) {
            Ok(name) => {
                if !entry.tags.contains(&name) && !add.contains(&name) {
                    add.push(name);
                }
            }
            Err(_) if settings.create_tags => {
                if !entry.tags.contains(t) {
                    create.push(t.clone());
                }
            }
            Err(_) => left_out.push(t.clone()),
        }
    }
    let rating = if settings.fill_rating && entry.rating.is_none() { post.rating } else { None };
    let source = if settings.fill_source && entry.source.is_none() { Some(post.source.clone().unwrap_or_else(|| post.post_url.clone())) } else { None };
    Change { site: post.site, post_id: post.id, post_url: post.post_url.clone(), add, create, left_out, rating, source }
}

/// Applies a [`plan`]: creates its new tags in the tag list, adds the tags (with what they
/// imply) and fills the rating and source. The caller saves the tag list, the collection and
/// the sidecars. Returns whether the file changed.
pub fn apply(collection: &mut Collection, tags: &mut TagList, hash: &str, change: &Change) -> Result<bool, StoreError> {
    let mut names: Vec<String> = change.add.clone();
    for t in &change.create {
        match tags.create(t, "") {
            Ok(name) => names.push(name),
            // Created meanwhile (by an earlier file in the same import): use it.
            Err(StoreError::TagExists(_)) => names.push(tags.resolve(t)?),
            Err(e) => return Err(e),
        }
    }
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut changed = collection.edit_tags(hash, &refs, &[], tags)?;
    if let Some(entry) = collection.get_mut(hash) {
        if let (Some(r), None) = (change.rating, entry.rating) {
            entry.rating = Some(r);
            changed = true;
        }
        if let (Some(s), None) = (&change.source, &entry.source) {
            entry.source = Some(s.clone());
            changed = true;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boorucraft_store::Kind;

    fn post(tags: &[&str]) -> Post {
        Post {
            site: Site::Danbooru,
            id: 7,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            skipped_tags: vec!["*bad".into()],
            rating: Some(Rating::Questionable),
            source: None,
            post_url: "https://danbooru.donmai.us/posts/7".into(),
        }
    }

    #[test]
    fn plans_only_additions_and_resolves_aliases() {
        let mut tags = TagList::in_memory();
        tags.create("cat", "").unwrap();
        tags.create("sky", "").unwrap();
        tags.add_alias("kitty", "cat").unwrap();
        let mut entry = FileEntry::new("m".into(), Kind::Image, "png".into(), 1);
        entry.tags.insert("sky".into());
        entry.source = Some("mine".into());
        let settings = BooruSettings::default();

        let c = plan(&post(&["kitty", "sky", "new_tag"]), &entry, &tags, &settings);
        assert_eq!(c.add, ["cat"], "alias resolved; sky already there");
        assert_eq!(c.create, ["new_tag"]);
        assert_eq!(c.left_out, ["*bad"]);
        assert_eq!(c.rating, Some(Rating::Questionable), "no rating yet");
        assert_eq!(c.source, None, "keeps the user's source");

        let off = BooruSettings { create_tags: false, fill_rating: false, ..BooruSettings::default() };
        let c = plan(&post(&["new_tag"]), &FileEntry::new("m".into(), Kind::Image, "png".into(), 1), &tags, &off);
        assert!(c.create.is_empty() && c.left_out.contains(&"new_tag".to_string()));
        assert_eq!(c.rating, None);
        assert_eq!(c.source.as_deref(), Some("https://danbooru.donmai.us/posts/7"), "no source on the post: the post itself");
        assert!(!c.is_empty());
    }

    #[test]
    fn apply_creates_tags_and_never_removes() {
        let dir = tempfile::tempdir().unwrap();
        let mut tags = TagList::in_memory();
        tags.create("cat", "").unwrap();
        tags.create("animal", "").unwrap();
        tags.add_implication("cat", "animal").unwrap();
        let mut c = Collection::open(dir.path()).unwrap();
        let mut entry = FileEntry::new("m".into(), Kind::Image, "png".into(), 1);
        entry.tags.insert("mine".into());
        entry.rating = Some(Rating::Safe);
        c.insert("h".into(), entry.clone());

        let change = plan(&post(&["cat", "brand_new"]), &entry, &tags, &BooruSettings::default());
        assert!(apply(&mut c, &mut tags, "h", &change).unwrap());
        let e = c.get("h").unwrap();
        assert_eq!(e.tags.iter().map(String::as_str).collect::<Vec<_>>(), ["animal", "brand_new", "cat", "mine"], "implied tag too; own tags kept");
        assert_eq!(e.rating, Some(Rating::Safe), "own rating kept");
        assert_eq!(e.source.as_deref(), Some("https://danbooru.donmai.us/posts/7"));
        assert!(tags.contains("brand_new"));
        // Again: nothing to do.
        let again = plan(&post(&["cat", "brand_new"]), c.get("h").unwrap(), &tags, &BooruSettings::default());
        assert!(again.is_empty(), "{again:?}");
        assert!(!apply(&mut c, &mut tags, "h", &again).unwrap());
    }

    #[test]
    fn settings_round_trip_privately_and_pick_sites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("booru.json");
        assert_eq!(BooruSettings::load(&path).unwrap(), BooruSettings::default());
        let defaults = BooruSettings::default();
        assert_eq!(defaults.active_sites(), [Site::Danbooru, Site::Safebooru], "Gelbooru waits for an account");

        let mut s = BooruSettings { sites: vec!["gelbooru".into(), "nope".into(), "safebooru".into(), "gelbooru".into()], ..BooruSettings::default() };
        s.accounts.insert("gelbooru".into(), Account { user: "1".into(), api_key: "secret".into() });
        assert_eq!(s.active_sites(), [Site::Gelbooru, Site::Safebooru]);
        assert!(!format!("{s:?}").contains("secret"));
        s.save(&path).unwrap();
        assert_eq!(BooruSettings::load(&path).unwrap(), s);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(&path, "{ not json").unwrap();
        assert!(BooruSettings::load(&path).is_err());
    }

    #[test]
    fn lookups_remember_answers_for_a_while() {
        let now = Utc::now();
        let sites = [Site::Danbooru, Site::Safebooru];
        let mut l = Lookups::default();
        assert!(!l.can_skip("m", &sites, now));
        l.record("m", Site::Danbooru, None, now);
        assert!(!l.can_skip("m", &sites, now), "Safebooru not asked yet");
        l.record("m", Site::Safebooru, None, now);
        assert!(l.can_skip("m", &sites, now), "nobody has it");
        assert!(!l.can_skip("m", &sites, now + Duration::days(LOOKUP_TTL_DAYS + 1)), "stale");
        // Found on the first site: skipped only once imported (a dry run doesn't count).
        l.record("n", Site::Danbooru, Some(5), now);
        assert!(!l.can_skip("n", &sites, now));
        l.mark_imported("n", Site::Danbooru);
        assert!(l.can_skip("n", &sites, now));
        assert!(!l.can_skip("n", &[], now));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c").join("booru-lookups.json");
        l.save(&path).unwrap();
        assert_eq!(Lookups::load(&path), l);
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(Lookups::load(&path), Lookups::default(), "a broken cache is just empty");
    }
}
