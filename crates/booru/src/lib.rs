//! Booru lookups: find a picture's post on Safebooru, Gelbooru or Danbooru by its MD5 and read
//! the post's tags, rating and source.
//!
//! [`parse_response`] turns a site's JSON into a [`Post`] without any network, so it is what the
//! tests exercise. [`Client`] adds HTTPS: rustls with the RustCrypto provider (no C or assembly
//! crypto), Mozilla's root certificates, an honest user agent naming BooruCraft, a pause of at
//! least [`MIN_INTERVAL`] between requests, timeouts and a cap on response size. Sites that want
//! an API key (Gelbooru) get one from [`Credentials`]; nothing here gets around bot protection.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use boorucraft_core::{Rating, normalize_tag};
use serde_json::Value;
use thiserror::Error;

/// The user agent every request sends: who is asking, and where to find out more.
pub const USER_AGENT: &str = concat!("BooruCraft/", env!("CARGO_PKG_VERSION"), " (+https://github.com/Yuki-init/boorucraft)");
/// The shortest gap between two requests from one [`Client`]. The sites allow more; this keeps
/// a big collection's import gentle on them.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
/// The largest response read. A lookup by MD5 returns one post, a few KiB.
pub const MAX_RESPONSE: u64 = 4 << 20;
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum BooruError {
    #[error("{0} needs an API key and user id (set them in the booru settings)")]
    NeedsKey(Site),
    #[error("{site} refused the request (HTTP {status}): check the API key and user id")]
    Refused { site: Site, status: u16 },
    #[error("{0} asked us to slow down (HTTP 429); try again later")]
    RateLimited(Site),
    #[error("{site} answered HTTP {status}")]
    Http { site: Site, status: u16 },
    #[error("couldn't reach {site}: {message}")]
    Network { site: Site, message: String },
    #[error("{site} sent a response BooruCraft can't read: {message}")]
    BadResponse { site: Site, message: String },
    #[error("`{0}` isn't an MD5 (32 hex digits)")]
    BadMd5(String),
}

pub type Result<T> = std::result::Result<T, BooruError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Site {
    Safebooru,
    Gelbooru,
    Danbooru,
}

impl Site {
    pub const ALL: [Site; 3] = [Site::Safebooru, Site::Gelbooru, Site::Danbooru];

    /// The lowercase name used on the command line and in settings.
    pub fn id(self) -> &'static str {
        match self {
            Site::Safebooru => "safebooru",
            Site::Gelbooru => "gelbooru",
            Site::Danbooru => "danbooru",
        }
    }

    pub fn parse(s: &str) -> Option<Site> {
        Site::ALL.into_iter().find(|site| site.id().eq_ignore_ascii_case(s.trim()))
    }

    /// Whether the site's API answers only with an API key and user id.
    pub fn needs_key(self) -> bool {
        self == Site::Gelbooru
    }

    fn base(self) -> &'static str {
        match self {
            Site::Safebooru => "https://safebooru.org",
            Site::Gelbooru => "https://gelbooru.com",
            Site::Danbooru => "https://danbooru.donmai.us",
        }
    }

    /// The API address that finds the post with this MD5. `md5` must already be checked.
    fn lookup_url(self, md5: &str, credentials: Option<&Credentials>) -> String {
        let mut url = match self {
            Site::Safebooru | Site::Gelbooru => format!("{}/index.php?page=dapi&s=post&q=index&json=1&limit=1&tags=md5:{md5}", self.base()),
            Site::Danbooru => format!("{}/posts.json?limit=1&tags=md5:{md5}", self.base()),
        };
        if let Some(c) = credentials.filter(|c| !c.is_empty()) {
            let (user, key) = match self {
                Site::Danbooru => ("login", "api_key"),
                Site::Safebooru | Site::Gelbooru => ("user_id", "api_key"),
            };
            url.push_str(&format!("&{user}={}&{key}={}", encode(&c.user), encode(&c.api_key)));
        }
        url
    }

    /// The page people look at for post `id`.
    pub fn post_url(self, id: u64) -> String {
        match self {
            Site::Safebooru | Site::Gelbooru => format!("{}/index.php?page=post&s=view&id={id}", self.base()),
            Site::Danbooru => format!("{}/posts/{id}", self.base()),
        }
    }
}

impl std::fmt::Display for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Site::Safebooru => "Safebooru",
            Site::Gelbooru => "Gelbooru",
            Site::Danbooru => "Danbooru",
        })
    }
}

/// An account on a site: Gelbooru's user id and API key, or Danbooru's login and API key.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    pub user: String,
    pub api_key: String,
}

impl Credentials {
    pub fn is_empty(&self) -> bool {
        self.user.trim().is_empty() || self.api_key.trim().is_empty()
    }
}

// Never print the key, even in debug output.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").field("user", &self.user).field("api_key", &"…").finish()
    }
}

/// What a site knows about a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub site: Site,
    pub id: u64,
    /// Tag names as BooruCraft writes them (lowercase, `_` between words), sorted, no repeats.
    pub tags: Vec<String>,
    /// Tags the site sent that aren't valid BooruCraft tag names (they are left out of `tags`).
    pub skipped_tags: Vec<String>,
    pub rating: Option<Rating>,
    /// Where the picture came from, as the post gives it (often the artist's page).
    pub source: Option<String>,
    /// The post on the booru.
    pub post_url: String,
}

/// A lowercase 32-digit hex MD5, or an error.
pub fn check_md5(md5: &str) -> Result<String> {
    let m = md5.trim().to_ascii_lowercase();
    if m.len() == 32 && m.bytes().all(|b| b.is_ascii_hexdigit()) { Ok(m) } else { Err(BooruError::BadMd5(md5.to_string())) }
}

/// Reads a lookup response. `Ok(None)`: the site has no post with that MD5.
pub fn parse_response(site: Site, md5: &str, body: &str) -> Result<Option<Post>> {
    let bad = |message: String| BooruError::BadResponse { site, message };
    // Safebooru answers an empty body when nothing matches.
    if body.trim().is_empty() {
        return Ok(None);
    }
    let json: Value = serde_json::from_str(body).map_err(|e| bad(format!("not JSON ({e})")))?;
    let posts = match (&json, site) {
        (Value::Array(posts), _) => posts.as_slice(),
        // Gelbooru: {"@attributes": {...}, "post": [...]}, without "post" when nothing matches.
        (Value::Object(obj), Site::Gelbooru) => match obj.get("post") {
            Some(Value::Array(posts)) => posts.as_slice(),
            Some(_) => return Err(bad("`post` isn't a list".into())),
            None => &[],
        },
        // Danbooru reports errors as an object with a message.
        (Value::Object(obj), _) => {
            let message = obj.get("message").and_then(Value::as_str).unwrap_or("unexpected object");
            return Err(bad(message.to_string()));
        }
        _ => return Err(bad("unexpected JSON".into())),
    };
    // Only a post whose MD5 matches counts, in case a site ignores the md5: search.
    let Some(post) = posts.iter().find(|p| p.get("md5").or_else(|| p.get("hash")).and_then(Value::as_str).is_some_and(|m| m.eq_ignore_ascii_case(md5))) else {
        return Ok(None);
    };
    let id = post.get("id").and_then(number).ok_or_else(|| bad("post has no id".into()))?;
    let raw_tags = match site {
        Site::Danbooru => text(post, "tag_string"),
        Site::Safebooru | Site::Gelbooru => text(post, "tags").map(|t| unescape_html(&t)),
    }
    .unwrap_or_default();
    let mut tags = Vec::new();
    let mut skipped_tags = Vec::new();
    for raw in raw_tags.split_whitespace() {
        match normalize_tag(raw) {
            Ok(t) => tags.push(t),
            Err(_) => skipped_tags.push(raw.to_string()),
        }
    }
    tags.sort();
    tags.dedup();
    let rating = text(post, "rating").and_then(|r| rating(&r));
    let source = text(post, "source").map(|s| unescape_html(s.trim())).filter(|s| !s.is_empty());
    Ok(Some(Post { site, id, tags, skipped_tags, rating, source, post_url: site.post_url(id) }))
}

/// A booru rating as BooruCraft's: general and safe are safe; sensitive and questionable are
/// questionable; explicit is explicit.
fn rating(r: &str) -> Option<Rating> {
    match r.trim().to_ascii_lowercase().as_str() {
        "g" | "general" | "s" | "safe" => Some(Rating::Safe),
        "sensitive" | "q" | "questionable" => Some(Rating::Questionable),
        "e" | "explicit" => Some(Rating::Explicit),
        _ => None,
    }
}

/// A string field (Gelbooru sends numbers as strings, Safebooru as numbers).
fn text(post: &Value, key: &str) -> Option<String> {
    match post.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn number(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Safebooru and Gelbooru send tags HTML-escaped (`hand_on_another&#039;s_face`).
fn unescape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let decoded = after.find(';').filter(|&end| end <= 10).and_then(|end| {
            let entity = &after[1..end];
            let c = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, end + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &after[len..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Percent-encodes a query value.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.trim().bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Makes the requests. Keep one for a whole import: it spaces its requests out.
pub struct Client {
    agent: ureq::Agent,
    last_request: Option<Instant>,
    interval: Duration,
}

impl Client {
    pub fn new() -> Client {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::Rustls)
            .root_certs(ureq::tls::RootCerts::WebPki)
            .unversioned_rustls_crypto_provider(Arc::new(rustls_rustcrypto::provider()))
            .build();
        let agent =
            ureq::Agent::config_builder().tls_config(tls).user_agent(USER_AGENT).timeout_global(Some(TIMEOUT)).http_status_as_error(false).build().new_agent();
        Client { agent, last_request: None, interval: MIN_INTERVAL }
    }

    /// Looks `md5` up on `site`. `Ok(None)`: the site has no such post.
    pub fn lookup(&mut self, site: Site, md5: &str, credentials: Option<&Credentials>) -> Result<Option<Post>> {
        let md5 = check_md5(md5)?;
        if site.needs_key() && credentials.is_none_or(Credentials::is_empty) {
            return Err(BooruError::NeedsKey(site));
        }
        self.wait();
        let url = site.lookup_url(&md5, credentials);
        self.last_request = Some(Instant::now());
        // Errors say which site failed but never repeat the URL: it can hold the API key.
        let network = |e: ureq::Error| BooruError::Network { site, message: describe(&e) };
        let mut response = self.agent.get(&url).header("Accept", "application/json").call().map_err(network)?;
        let status = response.status().as_u16();
        match status {
            200 => {}
            401 | 403 => return Err(BooruError::Refused { site, status }),
            429 => return Err(BooruError::RateLimited(site)),
            _ => return Err(BooruError::Http { site, status }),
        }
        let body = response.body_mut().with_config().limit(MAX_RESPONSE).read_to_string().map_err(network)?;
        parse_response(site, &md5, &body)
    }

    fn wait(&self) {
        if let Some(last) = self.last_request {
            let since = last.elapsed();
            if since < self.interval {
                thread::sleep(self.interval - since);
            }
        }
    }
}

impl Default for Client {
    fn default() -> Self {
        Client::new()
    }
}

/// A network error without the request URL (it can hold an API key).
fn describe(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Timeout(_) => "timed out".into(),
        ureq::Error::HostNotFound => "host not found (offline?)".into(),
        ureq::Error::ConnectionFailed => "connection failed".into(),
        ureq::Error::Io(io) => io.to_string(),
        ureq::Error::BodyExceedsLimit(_) => "response too large".into(),
        ureq::Error::Tls(m) => format!("TLS: {m}"),
        ureq::Error::Rustls(r) => format!("TLS: {r}"),
        other => {
            let s = other.to_string();
            if s.contains("api_key") { "request failed".into() } else { s }
        }
    }
}

#[cfg(test)]
mod tests;
