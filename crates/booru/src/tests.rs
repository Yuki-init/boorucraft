use super::*;

// Real responses for one picture, saved from each site (no keys in them).
const MD5: &str = "7203e73ede0f13c44d93cf895b0d37e4";
const SAFEBOORU: &str = include_str!("../tests/fixtures/safebooru.json");
const GELBOORU: &str = include_str!("../tests/fixtures/gelbooru.json");
const GELBOORU_MISS: &str = include_str!("../tests/fixtures/gelbooru-miss.json");
const DANBOORU: &str = include_str!("../tests/fixtures/danbooru.json");

#[test]
fn reads_a_post_from_each_site() {
    let s = parse_response(Site::Safebooru, MD5, SAFEBOORU).unwrap().unwrap();
    assert_eq!(s.id, 7277089);
    assert_eq!(s.post_url, "https://safebooru.org/index.php?page=post&s=view&id=7277089");
    assert_eq!(s.rating, Some(Rating::Safe), "general is safe");
    assert!(s.tags.contains(&"1girl".to_string()) && s.tags.contains(&"western_dragon".to_string()));
    assert!(s.tags.contains(&"hand_on_another's_face".to_string()), "HTML entities are decoded");
    assert!(s.tags.windows(2).all(|w| w[0] < w[1]), "sorted, no repeats");
    assert!(s.skipped_tags.is_empty());

    let g = parse_response(Site::Gelbooru, MD5, GELBOORU).unwrap().unwrap();
    assert_eq!(g.id, 15183922, "ids come as strings on Gelbooru");
    assert_eq!(g.source.as_deref(), Some("https://www.pixiv.net/artworks/126630963"));
    assert_eq!(g.post_url, "https://gelbooru.com/index.php?page=post&s=view&id=15183922");

    let d = parse_response(Site::Danbooru, MD5, DANBOORU).unwrap().unwrap();
    assert_eq!(d.id, 12476305);
    assert_eq!(d.post_url, "https://danbooru.donmai.us/posts/12476305");
    assert_eq!(d.rating, Some(Rating::Safe));
    assert!(d.tags.contains(&"hibiki04014".to_string()) && d.tags.contains(&"hand_on_another's_face".to_string()));
    // The same picture has the same tags everywhere.
    assert_eq!(s.tags, d.tags);
}

#[test]
fn no_post_is_none() {
    assert_eq!(parse_response(Site::Safebooru, MD5, "").unwrap(), None, "Safebooru: empty body");
    assert_eq!(parse_response(Site::Danbooru, MD5, "[]").unwrap(), None);
    assert_eq!(parse_response(Site::Gelbooru, MD5, GELBOORU_MISS).unwrap(), None, "Gelbooru: no `post`");
    // A post for another MD5 doesn't count.
    assert_eq!(parse_response(Site::Danbooru, "00000000000000000000000000000000", DANBOORU).unwrap(), None);
}

#[test]
fn hostile_responses_are_errors_not_crashes() {
    for body in ["<html>Just a moment...</html>", "{", "42", "\"x\"", "{\"post\": 3}", "[{\"md5\": 5}]"] {
        for site in Site::ALL {
            let r = parse_response(site, MD5, body);
            assert!(matches!(r, Err(BooruError::BadResponse { .. }) | Ok(None)), "{site} {body}: {r:?}");
        }
    }
    assert!(
        matches!(parse_response(Site::Danbooru, MD5, r#"{"success":false,"message":"slow down"}"#), Err(BooruError::BadResponse { message, .. }) if message == "slow down")
    );
    // Matching post without an id, bad tags, an odd rating.
    let odd = format!(r#"[{{"md5":"{MD5}","tag_string":"ok -bad *x  Fine","rating":"z"}}]"#);
    assert!(parse_response(Site::Danbooru, MD5, &odd).is_err());
    let odd = format!(r#"[{{"id":1,"md5":"{MD5}","tag_string":"ok -bad *x  Fine","rating":"z","source":"  "}}]"#);
    let p = parse_response(Site::Danbooru, MD5, &odd).unwrap().unwrap();
    assert_eq!(p.tags, ["fine", "ok"]);
    assert_eq!(p.skipped_tags, ["-bad", "*x"]);
    assert_eq!((p.rating, p.source), (None, None));
}

#[test]
fn ratings_and_entities() {
    assert_eq!(rating("sensitive"), Some(Rating::Questionable));
    assert_eq!(rating("q"), Some(Rating::Questionable));
    assert_eq!(rating("Explicit"), Some(Rating::Explicit));
    assert_eq!(rating("g"), Some(Rating::Safe));
    assert_eq!(rating(""), None);
    assert_eq!(unescape_html("a&amp;b &lt;3 &#039; &#x41; &bogus; & &;"), "a&b <3 ' A &bogus; & &;");
    assert_eq!(unescape_html("&#1114112;"), "&#1114112;", "not a char");
}

#[test]
fn urls_carry_credentials_encoded() {
    let c = Credentials { user: "12 34".into(), api_key: "k&y=".into() };
    let url = Site::Gelbooru.lookup_url(MD5, Some(&c));
    assert!(url.starts_with("https://gelbooru.com/index.php?page=dapi&s=post&q=index&json=1&limit=1&tags=md5:7203e73ede0f13c44d93cf895b0d37e4"));
    assert!(url.ends_with("&user_id=12%2034&api_key=k%26y%3D"));
    assert!(Site::Danbooru.lookup_url(MD5, Some(&c)).contains("&login=12%2034&api_key="));
    assert!(!Site::Safebooru.lookup_url(MD5, None).contains("api_key"));
    assert!(!Site::Danbooru.lookup_url(MD5, Some(&Credentials::default())).contains("login"), "empty credentials aren't sent");
    assert!(!format!("{c:?}").contains("k&y"), "debug output hides the key");
}

#[test]
fn lookups_check_inputs_before_the_network() {
    let mut client = Client::new();
    assert!(matches!(client.lookup(Site::Safebooru, "nope", None), Err(BooruError::BadMd5(_))));
    assert!(matches!(client.lookup(Site::Gelbooru, MD5, None), Err(BooruError::NeedsKey(Site::Gelbooru))));
    assert_eq!(check_md5(" 7203E73EDE0F13C44D93CF895B0D37E4 ").unwrap(), MD5);
    assert_eq!(Site::parse("Danbooru"), Some(Site::Danbooru));
    assert_eq!(Site::parse("e621"), None);
}

/// Real lookups over HTTPS (checks the TLS stack against the sites). Not run by default; with
/// network: `cargo test -p boorucraft-booru -- --ignored`.
#[test]
#[ignore = "needs network"]
fn live_lookups() {
    let mut client = Client::new();
    for site in [Site::Safebooru, Site::Danbooru] {
        let post = client.lookup(site, MD5, None).unwrap().unwrap();
        assert!(post.tags.contains(&"1girl".to_string()), "{site}");
        assert_eq!(client.lookup(site, "00000000000000000000000000000000", None).unwrap(), None, "{site}");
    }
    // Gelbooru only with a key: GELBOORU_USER_ID and GELBOORU_API_KEY.
    let key = Credentials { user: std::env::var("GELBOORU_USER_ID").unwrap_or_default(), api_key: std::env::var("GELBOORU_API_KEY").unwrap_or_default() };
    if !key.is_empty() {
        let post = client.lookup(Site::Gelbooru, MD5, Some(&key)).unwrap().unwrap();
        assert!(post.tags.contains(&"1girl".to_string()));
        let wrong = Credentials { user: key.user.clone(), api_key: "wrong".into() };
        assert!(matches!(client.lookup(Site::Gelbooru, MD5, Some(&wrong)), Err(BooruError::Refused { .. })));
    }
}
