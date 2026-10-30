//! `boorucraft-cli`: BooruCraft without a window.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use boorucraft_core::{Query, Rating};
use boorucraft_index::{ScanCache, SidecarReport, find_groups, needing_hashes, scan, set_hashes, write_sidecars};
use boorucraft_store::{Collection, Links, SearchIndex, StoreError, TagList, paths};
use rayon::prelude::*;

const USAGE: &str = "usage:
  boorucraft-cli scan DIR                    index a folder (creates DIR/collection.json)
  boorucraft-cli search DIR [QUERY...]       list matching files (see Searches below)
  boorucraft-cli info DIR [FILE]             collection summary, or one file's details
  boorucraft-cli tag DIR FILE [TAG...] [--rating s|q|e|none] [--source URL|none] [--parent FILE|none] [--create]
                                             TAG adds, -TAG removes; --create adds new tags to the list;
                                             --parent links FILE as a version of another file
  boorucraft-cli dupes DIR... [--threshold N] find near-duplicates (hashes missing pictures first;
                                             N is the most differing bits, default 6)
  boorucraft-cli unknown DIR                 tags used in DIR that aren't in the tag list
  boorucraft-cli recover DIR --from-backup|--from-sidecars
                                             reopen a collection whose collection.json is damaged,
                                             from collection.json.bak or by reading the sidecars
                                             (a copy of the damaged file goes to the config folder)
  boorucraft-cli forget-missing DIR [--yes]  list files missing since the last scan; with --yes,
                                             remove them and their tags (no file is touched)
  boorucraft-cli tags [PREFIX]               list tags (with descriptions)
  boorucraft-cli tags create TAG...
  boorucraft-cli tags describe TAG TEXT
  boorucraft-cli tags rename OLD NEW
  boorucraft-cli tags merge INTO FROM...
  boorucraft-cli tags delete TAG...
  boorucraft-cli pools                       list pools (named, ordered groups)
  boorucraft-cli pools create|delete NAME
  boorucraft-cli pools rename OLD NEW
  boorucraft-cli pools add DIR NAME FILE...  append files to a pool, in this order
  boorucraft-cli pools remove DIR NAME FILE...
  boorucraft-cli tags namespaces
  boorucraft-cli tags add-namespace NAME [COLOR]
  boorucraft-cli tags aliases | implications list them
  boorucraft-cli tags alias ALIAS TAG        ALIAS stands for TAG when tagging and searching
                                             (an existing tag ALIAS is merged into TAG)
  boorucraft-cli tags unalias ALIAS...
  boorucraft-cli tags imply TAG IMPLIED...   files given TAG also get IMPLIED (files that have
                                             TAG already get it as collections are next opened)
  boorucraft-cli tags unimply TAG IMPLIED... (files keep the tags they have)

Searches: cat sky (both)  -dog (not)  ~cat ~dog (either)  cat* (wildcard)
  rating:s,q  width:>1920  height:<=1080  ratio:16:9  mpixels:>2  filesize:<2MB
  type:png,gif  kind:video  animated:yes  missing:yes  dupes:yes  parent:3fa9  child:yes  pool:name  tagcount:0  md5:…  hash:3fa9
  date:2024-01..2024-06  date:>2023-05-01  imported:<7d  path:\"holiday 2023\"  source:none
  order:date  order:-filesize  order:name  order:random  (default: newest imported first)
Environment: BOORUCRAFT_TAGS (tag list file), BOORUCRAFT_CONFIG_DIR, BOORUCRAFT_CACHE_DIR
 boorucraft-cli --version prints the version.";

/// Set once a reader closes stdout or stderr (e.g. `boorucraft-cli info DIR | head`): from then
/// on output to it is dropped. `println!` would panic instead, and the command must still finish
/// (a `tag` that prints and then saves has to save).
static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);
static STDERR_CLOSED: AtomicBool = AtomicBool::new(false);

/// `println!` that never panics.
macro_rules! out {
    ($($arg:tt)*) => { write_line(&mut std::io::stdout().lock(), &STDOUT_CLOSED, format_args!($($arg)*)) };
}

/// `eprintln!` that never panics.
macro_rules! err {
    ($($arg:tt)*) => { write_line(&mut std::io::stderr().lock(), &STDERR_CLOSED, format_args!($($arg)*)) };
}

/// Writes one line; any write error (normally a closed pipe) stops all later output to `w`.
fn write_line(w: &mut impl Write, closed: &AtomicBool, args: std::fmt::Arguments) {
    if closed.load(Ordering::Relaxed) {
        return;
    }
    if writeln!(w, "{args}").and_then(|()| w.flush()).is_err() {
        closed.store(true, Ordering::Relaxed);
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            err!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

type Res<T = ()> = Result<T, String>;

fn run(args: &[String]) -> Res {
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();
    match strs.as_slice() {
        ["scan", dir] => cmd_scan(Path::new(dir)),
        ["search", dir, query @ ..] => cmd_search(Path::new(dir), &query.join(" ")),
        ["info", dir] => cmd_info(Path::new(dir), None),
        ["info", dir, file] => cmd_info(Path::new(dir), Some(Path::new(file))),
        ["tag", dir, file, rest @ ..] if !rest.is_empty() => cmd_tag(Path::new(dir), Path::new(file), rest),
        ["unknown", dir] => cmd_unknown(Path::new(dir)),
        ["recover", dir, "--from-backup"] => cmd_recover(Path::new(dir), boorucraft_store::Recovery::Backup),
        ["recover", dir, "--from-sidecars"] => cmd_recover(Path::new(dir), boorucraft_store::Recovery::Empty),
        ["forget-missing", dir] => cmd_forget_missing(Path::new(dir), false),
        ["forget-missing", dir, "--yes"] => cmd_forget_missing(Path::new(dir), true),
        ["dupes", rest @ ..] if !rest.is_empty() => cmd_dupes(rest),
        ["tags", rest @ ..] => cmd_tags(rest),
        ["pools", rest @ ..] => cmd_pools(rest),
        ["--version" | "-V" | "version"] => {
            out!("boorucraft-cli {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["help" | "--help" | "-h"] => {
            out!("{USAGE}");
            Ok(())
        }
        _ => Err(format!("unrecognised command\n\n{USAGE}")),
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn open_tags(write: bool) -> Res<TagList> {
    let path = paths::tag_list_path();
    if write { TagList::open(&path) } else { TagList::open_read_only(&path) }.map_err(err)
}

/// Opens a collection, catches it up with the tag list, and rewrites the sidecars of files
/// whose tags that changed.
fn open_collection(dir: &Path, write: bool, tags: &TagList) -> Res<(Collection, ScanCache)> {
    let mut c = if write { Collection::open(dir) } else { Collection::open_read_only(dir) }.map_err(|e| match e {
        StoreError::Json { .. } => {
            let hint = if Collection::backup_is_readable(dir) { "--from-backup (or --from-sidecars)" } else { "--from-sidecars" };
            format!("{e}\nIt can be recovered with `boorucraft-cli recover {} {hint}`", dir.display())
        }
        e => e.to_string(),
    })?;
    let mut cache = ScanCache::load(&paths::cache_dir(), c.root());
    let changed = c.apply_tag_history(tags);
    if write && !changed.is_empty() {
        err!("updated {} file(s) for tag renames, merges and deletes", changed.len());
        let mut report = SidecarReport::default();
        write_sidecars(&c, tags, changed.iter().map(String::as_str), &mut cache, &mut report);
        print_errors(&report.errors);
    }
    Ok((c, cache))
}

fn save(c: &mut Collection, cache: &ScanCache) -> Res {
    c.save().map_err(err)?;
    if let Err(e) = cache.save() {
        err!("warning: couldn't save the scan cache: {e}");
    }
    Ok(())
}

fn print_errors(errors: &[(PathBuf, String)]) {
    for (p, e) in errors {
        err!("  {}: {e}", p.display());
    }
}

fn cmd_scan(dir: &Path) -> Res {
    let tags = open_tags(false)?;
    let (mut c, mut cache) = open_collection(dir, true, &tags)?;
    let r = scan(&mut c, &tags, &mut cache);
    save(&mut c, &cache)?;
    out!(
        "{} files ({} hashed): {} new, {} moved or copied, {} edited, {} missing; sidecars: {} read back, {} written",
        r.files,
        r.hashed,
        r.added,
        r.moved,
        r.edited,
        r.missing,
        r.sidecars.read,
        r.sidecars.written
    );
    if !r.errors.is_empty() || !r.sidecars.errors.is_empty() {
        err!("{} problem(s):", r.errors.len() + r.sidecars.errors.len());
        print_errors(&r.errors);
        print_errors(&r.sidecars.errors);
    }
    let unknown = c.unknown_tags(&tags);
    if !unknown.is_empty() {
        err!("{} tag(s) aren't in the tag list; see `boorucraft-cli unknown {}`", unknown.len(), dir.display());
    }
    Ok(())
}

fn cmd_search(dir: &Path, query: &str) -> Res {
    let mut q = Query::parse(query).map_err(|e| {
        let pad = " ".repeat(query.get(..e.span.start).map_or(0, |s| s.chars().count()));
        let marks = "^".repeat(query.get(e.span.clone()).map_or(1, |s| s.chars().count().max(1)));
        format!("{e}\n  {query}\n  {pad}{marks}")
    })?;
    let tags = open_tags(false)?;
    tags.resolve_query_aliases(&mut q);
    let (c, _) = open_collection(dir, false, &tags)?;
    let index = SearchIndex::build(&c);
    let mut links = links_for(&c, &q);
    links.parents = index.parents().map(str::to_string).collect();
    let hits = index.search(&q, &links);
    for hash in &hits {
        let Some(e) = c.get(hash) else { continue };
        let Some(rel) = e.paths.first() else { continue };
        let shown = c.abs_path(rel).map_or_else(|| rel.clone(), |p| p.display().to_string());
        if e.missing {
            out!("{shown}  [missing]");
        } else {
            out!("{shown}");
        }
    }
    err!("{} of {} file(s)", hits.len(), index.len());
    Ok(())
}

/// What a query needs to know about files beyond their own entry: only worked out when the
/// query uses it. Uses the perceptual hashes already stored (`dupes` computes missing ones).
fn links_for(c: &Collection, q: &Query) -> Links {
    let mut links = Links::default();
    if q.terms.iter().any(|t| matches!(t.kind, boorucraft_core::TermKind::Dupes(_))) {
        links.dupes = find_groups(&[Some(c)], boorucraft_dedup::DEFAULT_THRESHOLD).into_iter().flatten().map(|(_, h)| h).collect();
    }
    links
}

fn cmd_dupes(rest: &[&str]) -> Res {
    let mut dirs = Vec::new();
    let mut threshold = boorucraft_dedup::DEFAULT_THRESHOLD;
    let mut it = rest.iter();
    while let Some(&a) = it.next() {
        if a == "--threshold" {
            let v = it.next().ok_or("--threshold needs a number")?;
            threshold = v
                .parse()
                .ok()
                .filter(|n| *n <= boorucraft_dedup::MAX_THRESHOLD)
                .ok_or_else(|| format!("--threshold takes 0 to {}", boorucraft_dedup::MAX_THRESHOLD))?;
        } else if a.starts_with("--") {
            return Err(format!("unknown option {a}"));
        } else {
            dirs.push(Path::new(a));
        }
    }
    let tags = open_tags(false)?;
    let thumbs = boorucraft_thumbs::ThumbCache::new(&paths::cache_dir());
    let mut colls = Vec::new();
    for dir in dirs {
        let (mut c, cache) = open_collection(dir, true, &tags)?;
        let todo: Vec<_> = needing_hashes(&c).into_iter().filter(|(_, _, f)| boorucraft_thumbs::can_decode(f)).collect();
        if !todo.is_empty() {
            err!("{}: hashing {} picture(s)…", dir.display(), todo.len());
            let results: Vec<_> = todo.par_iter().map(|(hash, path, format)| (hash, path, thumbs.thumbnail_and_hashes(hash, path, format))).collect();
            for (hash, path, r) in results {
                match r {
                    Ok((_, h)) => set_hashes(&mut c, hash, h),
                    Err(e) => err!("  {}: {e}", path.display()),
                }
            }
            save(&mut c, &cache)?;
        }
        colls.push(c);
    }
    let refs: Vec<Option<&Collection>> = colls.iter().map(Some).collect();
    let groups = find_groups(&refs, threshold);
    for (n, g) in groups.iter().enumerate() {
        out!("group {} ({} files)", n + 1, g.len());
        for (ci, hash) in g {
            let Some(c) = colls.get(*ci) else { continue };
            let Some(e) = c.get(hash) else { continue };
            let shown = e.paths.first().and_then(|p| c.abs_path(p)).map_or_else(|| hash.clone(), |p| p.display().to_string());
            let size = match (e.width, e.height) {
                (Some(w), Some(h)) => format!("{w}x{h}, "),
                _ => String::new(),
            };
            out!("  {shown}  ({size}{} bytes)", e.size);
        }
    }
    err!("{} group(s) of near-duplicates", groups.len());
    Ok(())
}

fn find_file(c: &Collection, file: &Path) -> Res<String> {
    let abs = file.canonicalize().map_err(|e| format!("{}: {e}", file.display()))?;
    let rel = c.rel_path(&abs).ok_or_else(|| format!("{} is not inside {}", file.display(), c.root().display()))?;
    c.hash_of_path(&rel).map(str::to_string).ok_or_else(|| format!("{rel} isn't in the collection yet; run `boorucraft-cli scan` first"))
}

fn cmd_info(dir: &Path, file: Option<&Path>) -> Res {
    let tags = open_tags(false)?;
    let (c, _) = open_collection(dir, false, &tags)?;
    let Some(file) = file else {
        let index = SearchIndex::build(&c);
        let missing = c.files().filter(|(_, e)| e.missing).count();
        let untagged = c.files().filter(|(_, e)| e.tags.is_empty()).count();
        out!("{}", c.root().display());
        out!("  files: {} ({missing} missing, {untagged} untagged)", c.len());
        for (r, n) in index.rating_counts() {
            out!("  {r}: {n}");
        }
        out!("  top tags:");
        for (t, n) in index.tag_counts().into_iter().take(20) {
            out!("    {n:>6}  {t}");
        }
        return Ok(());
    };
    let hash = find_file(&c, file)?;
    let e = c.get(&hash).ok_or("file vanished")?;
    out!("sha256:   {hash}");
    out!("md5:      {}", e.md5);
    for p in &e.paths {
        out!("path:     {p}");
    }
    out!("kind:     {:?}, {}{}", e.kind, e.format, if e.animated { ", animated" } else { "" });
    if let (Some(w), Some(h)) = (e.width, e.height) {
        out!("size:     {w}x{h}, {} bytes", e.size);
    } else {
        out!("size:     {} bytes", e.size);
    }
    if let Some(t) = e.taken {
        out!("taken:    {t}");
    }
    out!("imported: {}", e.imported);
    out!("rating:   {}", e.rating.map_or_else(|| "-".to_string(), |r| r.to_string()));
    if let Some(s) = &e.source {
        out!("source:   {s}");
    }
    let tag_names: Vec<String> = e.tags.iter().map(|t| if tags.contains(t) { t.clone() } else { format!("{t} (unknown)") }).collect();
    out!("tags:     {}", tag_names.join(" "));
    if let Some(p) = &e.parent {
        let shown = c.get(p).and_then(|pe| pe.paths.first().cloned()).map_or_else(|| p.clone(), |path| format!("{path} ({p})"));
        out!("parent:   {shown}");
    }
    let children: Vec<&str> =
        c.files().filter(|(_, x)| x.parent.as_deref() == Some(hash.as_str())).filter_map(|(_, x)| x.paths.first().map(String::as_str)).collect();
    if !children.is_empty() {
        out!("children: {}", children.join(", "));
    }
    if e.missing {
        out!("missing:  yes");
    }
    Ok(())
}

fn cmd_tag(dir: &Path, file: &Path, rest: &[&str]) -> Res {
    let mut add = Vec::new();
    let mut remove = Vec::new();
    let mut rating: Option<Option<Rating>> = None;
    let mut source: Option<Option<String>> = None;
    let mut parent: Option<Option<&str>> = None;
    let mut create = false;
    let mut it = rest.iter();
    while let Some(&a) = it.next() {
        match a {
            "--create" => create = true,
            "--rating" => {
                let v = it.next().ok_or("--rating needs s, q, e or none")?;
                rating = Some(if *v == "none" { None } else { Some(Rating::parse(v).ok_or_else(|| format!("unknown rating `{v}`"))?) });
            }
            "--parent" => {
                let v = it.next().ok_or("--parent needs a file or none")?;
                parent = Some(if *v == "none" { None } else { Some(v) });
            }
            "--source" => {
                let v = it.next().ok_or("--source needs a URL or none")?;
                source = Some(if *v == "none" { None } else { Some(v.to_string()) });
            }
            _ if a.starts_with("--") => return Err(format!("unknown option {a}")),
            _ => match a.strip_prefix('-') {
                Some(t) => remove.push(t),
                None => add.push(a.strip_prefix('+').unwrap_or(a)),
            },
        }
    }

    let mut tags = open_tags(create)?;
    if create {
        for t in &add {
            match tags.create(t, "") {
                Ok(name) => err!("created tag `{name}`"),
                Err(StoreError::TagExists(_)) => {}
                Err(e) => return Err(err(e)),
            }
        }
        tags.save().map_err(err)?;
    }
    for t in &add {
        if let Err(StoreError::UnknownTag(name)) = tags.resolve(t) {
            let hint = tags.suggest(&name, 5);
            let hint = if hint.is_empty() { String::new() } else { format!(" Did you mean: {}?", hint.join(", ")) };
            return Err(format!("tag `{name}` isn't in the tag list.{hint} Use --create to add it."));
        }
    }

    let (mut c, mut cache) = open_collection(dir, true, &tags)?;
    let hash = find_file(&c, file)?;
    let parent_hash = match parent {
        Some(Some(p)) => {
            let ph = find_file(&c, Path::new(p))?;
            if ph == hash || ancestors(&c, &ph).contains(&hash) {
                return Err("a file can't be its own parent or ancestor".into());
            }
            Some(Some(ph))
        }
        Some(None) => Some(None),
        None => None,
    };
    c.edit_tags(&hash, &add, &remove, &tags).map_err(err)?;
    if let (Some(p), Some(e)) = (parent_hash, c.get_mut(&hash)) {
        e.parent = p;
    }
    if let Some(e) = c.get_mut(&hash) {
        if let Some(r) = rating {
            e.rating = r;
        }
        if let Some(s) = source {
            e.source = s;
        }
    }
    save(&mut c, &cache)?;
    let mut report = SidecarReport::default();
    write_sidecars(&c, &tags, [hash.as_str()], &mut cache, &mut report);
    print_errors(&report.errors);
    if let Err(e) = cache.save() {
        err!("warning: couldn't save the scan cache: {e}");
    }
    let e = c.get(&hash).ok_or("file vanished")?;
    out!("{}: {}", file.display(), e.tags.iter().cloned().collect::<Vec<_>>().join(" "));
    Ok(())
}

/// Parent, grandparent… of a file (stops at 100 or at a loop).
fn ancestors(c: &Collection, hash: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut at = c.get(hash).and_then(|e| e.parent.clone());
    while let Some(h) = at {
        if out.len() >= 100 || out.contains(&h) {
            break;
        }
        at = c.get(&h).and_then(|e| e.parent.clone());
        out.push(h);
    }
    out
}

fn cmd_unknown(dir: &Path) -> Res {
    let tags = open_tags(false)?;
    let (c, _) = open_collection(dir, false, &tags)?;
    let unknown = c.unknown_tags(&tags);
    if unknown.is_empty() {
        err!("every tag in this collection is in the tag list");
    }
    for (t, n) in unknown {
        let hint = tags.suggest(&t, 3);
        let hint = if hint.is_empty() { String::new() } else { format!("  (similar: {})", hint.join(", ")) };
        out!("{n:>6}  {t}{hint}");
    }
    Ok(())
}

fn cmd_recover(dir: &Path, from: boorucraft_store::Recovery) -> Res {
    let tags = open_tags(false)?;
    match Collection::open_read_only(dir) {
        Err(StoreError::Json { .. }) => {}
        Ok(_) => return Err("collection.json isn't damaged; nothing to recover".into()),
        Err(e) => return Err(e.to_string()),
    }
    let copy = Collection::copy_damaged(dir, &paths::config_dir().join("damaged")).map_err(err)?;
    err!("copied the damaged collection.json to {}", copy.display());
    let mut c = Collection::open_recovering(dir, from).map_err(err)?;
    c.apply_tag_history(&tags);
    let mut cache = ScanCache::load(&paths::cache_dir(), c.root());
    let r = scan(&mut c, &tags, &mut cache);
    save(&mut c, &cache)?;
    let tagged = c.files().filter(|(_, e)| !e.tags.is_empty()).count();
    out!("recovered {} files ({tagged} tagged); {} sidecar(s) read", c.len(), r.sidecars.read);
    Ok(())
}

fn cmd_forget_missing(dir: &Path, yes: bool) -> Res {
    let tags = open_tags(false)?;
    let (mut c, cache) = open_collection(dir, yes, &tags)?;
    let missing: Vec<String> = c.files().filter(|(_, e)| e.missing).map(|(h, _)| h.to_string()).collect();
    for h in &missing {
        let Some(e) = c.get(h) else { continue };
        let paths: Vec<&str> = e.paths.iter().map(String::as_str).collect();
        out!("{}  ({} tag{})", paths.join(", "), e.tags.len(), if e.tags.len() == 1 { "" } else { "s" });
    }
    if missing.is_empty() {
        err!("no missing files (as of the last scan)");
        return Ok(());
    }
    if !yes {
        err!("{} missing file(s); run again with --yes to forget them", missing.len());
        return Ok(());
    }
    let gone = c.forget_missing(missing.iter().map(String::as_str));
    save(&mut c, &cache)?;
    err!("forgot {} missing file(s)", gone.len());
    Ok(())
}

fn cmd_pools(rest: &[&str]) -> Res {
    match rest {
        [] => {
            let tags = open_tags(false)?;
            for (p, info) in tags.pools() {
                if info.description.is_empty() {
                    out!("{p}");
                } else {
                    out!("{p}  — {}", info.description);
                }
            }
            Ok(())
        }
        [cmd @ ("add" | "remove"), dir, name, files @ ..] if !files.is_empty() => {
            let tags = open_tags(false)?;
            let pool = tags.resolve_pool(name).map_err(err)?;
            let (mut c, cache) = open_collection(Path::new(dir), true, &tags)?;
            let hashes: Vec<String> = files.iter().map(|f| find_file(&c, Path::new(f))).collect::<Res<_>>()?;
            let mut members: Vec<(u32, String)> = c.files().filter_map(|(h, e)| e.pools.get(&pool).map(|n| (*n, h.to_string()))).collect();
            members.sort();
            let mut order: Vec<String> = members.into_iter().map(|(_, h)| h).collect();
            if *cmd == "add" {
                for h in hashes {
                    if !order.contains(&h) {
                        order.push(h);
                    }
                }
            } else {
                order.retain(|h| !hashes.contains(h));
                for h in &hashes {
                    if let Some(e) = c.get_mut(h) {
                        e.pools.remove(&pool);
                    }
                }
            }
            for (h, n) in order.iter().zip(1u32..) {
                if let Some(e) = c.get_mut(h) {
                    e.pools.insert(pool.clone(), n);
                }
            }
            save(&mut c, &cache)?;
            out!("{pool}: {} file(s) in this collection", order.len());
            Ok(())
        }
        [cmd, args @ ..] => {
            let mut tags = open_tags(true)?;
            match (*cmd, args) {
                ("create", [name]) => out!("created pool `{}`", tags.create_pool(name, "").map_err(err)?),
                ("rename", [from, to]) => out!("renamed to `{}`; collections update when next opened", tags.rename_pool(from, to).map_err(err)?),
                ("delete", [name]) => {
                    tags.delete_pool(name).map_err(err)?;
                    out!("deleted; collections update when next opened (the files stay)");
                }
                _ => return Err(format!("unrecognised pools command\n\n{USAGE}")),
            }
            tags.save().map_err(err)
        }
    }
}

/// `tags` subcommands that change the tag list.
const TAG_EDITS: &[&str] = &["create", "describe", "rename", "merge", "delete", "add-namespace", "alias", "unalias", "imply", "unimply"];

fn cmd_tags(rest: &[&str]) -> Res {
    match rest {
        ["namespaces"] => {
            let tags = open_tags(false)?;
            for (n, info) in tags.namespaces() {
                out!("{n}  {}", info.color);
            }
            Ok(())
        }
        ["aliases"] => {
            let tags = open_tags(false)?;
            for (a, t) in tags.aliases() {
                out!("{a} -> {t}");
            }
            Ok(())
        }
        ["implications"] => {
            let tags = open_tags(false)?;
            for (t, i) in tags.implications() {
                out!("{t} -> {i}");
            }
            Ok(())
        }
        [] | [_] if !rest.first().is_some_and(|c| TAG_EDITS.contains(c)) => {
            let tags = open_tags(false)?;
            let prefix = rest.first().map(|p| p.to_lowercase()).unwrap_or_default();
            for (t, info) in tags.tags().filter(|(t, _)| t.starts_with(&prefix)) {
                if info.description.is_empty() {
                    out!("{t}");
                } else {
                    out!("{t}  — {}", info.description);
                }
            }
            err!("{} tag(s) in {}", tags.len(), tags.path().display());
            Ok(())
        }
        [] => Ok(()),
        [cmd, args @ ..] => {
            let mut tags = open_tags(true)?;
            match (*cmd, args) {
                ("create", names) if !names.is_empty() => {
                    for n in names {
                        let name = tags.create(n, "").map_err(err)?;
                        out!("created `{name}`");
                    }
                }
                ("describe", [tag, text @ ..]) => tags.set_description(&tags.resolve(tag).map_err(err)?, &text.join(" ")).map_err(err)?,
                ("rename", [from, to]) => {
                    let to = tags.rename(from, to).map_err(err)?;
                    out!("renamed to `{to}`; collections update when next opened");
                }
                ("merge", [into, from @ ..]) if !from.is_empty() => {
                    tags.merge(from, into).map_err(err)?;
                    out!("merged into `{into}`; collections update when next opened");
                }
                ("delete", names) if !names.is_empty() => {
                    for n in names {
                        tags.delete(n).map_err(err)?;
                    }
                    out!("deleted; collections update when next opened");
                }
                ("alias", [alias, tag]) => {
                    let merging = tags.contains(&boorucraft_core::normalize_tag(alias).map_err(|e| e.to_string())?);
                    tags.add_alias(alias, tag).map_err(err)?;
                    if merging {
                        out!("`{alias}` merged into `{tag}` and kept as an alias; collections update when next opened");
                    }
                }
                ("unalias", names) if !names.is_empty() => {
                    for n in names {
                        tags.remove_alias(n).map_err(err)?;
                    }
                }
                ("imply", [tag, implied @ ..]) if !implied.is_empty() => {
                    for i in implied {
                        tags.add_implication(tag, i).map_err(err)?;
                    }
                    out!("files with `{tag}` get the implied tags as collections are next opened");
                }
                ("unimply", [tag, implied @ ..]) if !implied.is_empty() => {
                    for i in implied {
                        tags.remove_implication(tag, i).map_err(err)?;
                    }
                }
                ("add-namespace", [name]) => tags.add_namespace(name, "").map_err(err)?,
                ("add-namespace", [name, color]) => tags.add_namespace(name, color).map_err(err)?,
                _ => return Err(format!("unrecognised tags command\n\n{USAGE}")),
            }
            tags.save().map_err(err)
        }
    }
}
