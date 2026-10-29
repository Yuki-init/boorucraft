use super::*;

fn ns(n: &str) -> bool {
    matches!(n, "artist" | "character")
}

fn data(tags: &[&str]) -> SidecarData {
    SidecarData {
        tags: tags.iter().map(|t| t.to_string()).collect(),
        rating: Some(Rating::Questionable),
        source: Some("https://example.org/p?a=1&b=2".into()),
        hash: Some("abc123".into()),
    }
}

fn s(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

/// Shaped like a darktable sidecar: simple properties as attributes, an edit history, and
/// its own dc:subject.
const DARKTABLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="XMP Core 4.4.0-Exiv2">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:darktable="http://darktable.sf.net/"
    xmp:Rating="1"
    darktable:xmp_version="5">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>old_tag</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <darktable:history>
    <rdf:Seq>
     <rdf:li darktable:operation="exposure" darktable:params="0000a0c0 &amp; more"/>
    </rdf:Seq>
   </darktable:history>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
"#;

#[test]
fn new_sidecar_round_trips() {
    let d = data(&["cat", "artist:foo", "a&b <x>"]);
    let out = update(None, &d, &ns).unwrap();
    let back = read(&out, ns).unwrap();
    assert_eq!(back, d, "{}", s(&out));
    assert!(s(&out).contains("artist|foo"));
    assert!(s(&out).contains("a&amp;b &lt;x&gt;"));
}

#[test]
fn keeps_other_apps_fields() {
    let out = update(Some(DARKTABLE.as_bytes()), &data(&["cat"]), &ns).unwrap();
    let text = s(&out);
    assert!(text.contains(r#"xmp:Rating="1""#), "{text}");
    assert!(text.contains(r#"darktable:xmp_version="5""#));
    assert!(text.contains(r#"darktable:params="0000a0c0 &amp; more""#));
    assert!(text.contains("x:xmptk=\"XMP Core 4.4.0-Exiv2\""));
    assert!(!text.contains("old_tag"), "dc:subject is managed and replaced");
    assert_eq!(text.matches("<dc:subject>").count(), 1);
    assert_eq!(read(&out, ns).unwrap().tags, ["cat".to_string()].into());
}

#[test]
fn reads_other_apps_tags() {
    let back = read(DARKTABLE.as_bytes(), ns).unwrap();
    assert_eq!(back.tags, ["old_tag".to_string()].into());
    assert_eq!(back.rating, None, "xmp:Rating stars are not the booru rating");
}

#[test]
fn repeated_updates_do_not_pile_up() {
    let first = update(Some(DARKTABLE.as_bytes()), &data(&["cat"]), &ns).unwrap();
    let second = update(Some(&first), &data(&["dog"]), &ns).unwrap();
    let third = update(Some(&second), &data(&["dog"]), &ns).unwrap();
    assert_eq!(s(&second), s(&third), "updating with the same data changes nothing");
    let text = s(&third);
    assert_eq!(text.matches("rdf:Description ").count(), 2, "{text}");
    assert_eq!(text.matches("<boorucraft:Hash>").count(), 1);
    assert_eq!(read(&third, ns).unwrap().tags, ["dog".to_string()].into());
}

#[test]
fn empty_data_removes_our_description() {
    let with = update(Some(DARKTABLE.as_bytes()), &data(&["cat"]), &ns).unwrap();
    let without = update(Some(&with), &SidecarData::default(), &ns).unwrap();
    let text = s(&without);
    assert!(!text.contains("boorucraft"), "{text}");
    assert!(text.contains("darktable:history"));
    assert_eq!(text.matches("rdf:Description ").count(), 1);
}

#[test]
fn lightroom_hierarchy_becomes_namespaces() {
    let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
<dc:subject><rdf:Bag><rdf:li>foo</rdf:li><rdf:li>cat</rdf:li><rdf:li>Places</rdf:li></rdf:Bag></dc:subject>
<lr:hierarchicalSubject><rdf:Bag><rdf:li>Artist|foo</rdf:li><rdf:li>Places|Europe|France</rdf:li></rdf:Bag></lr:hierarchicalSubject>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;
    let back = read(xml.as_bytes(), ns).unwrap();
    let want: BTreeSet<String> = ["artist:foo", "cat"].iter().map(|t| t.to_string()).collect();
    assert_eq!(back.tags, want, "Places is only a parent level, not a tag");

    let out = update(Some(xml.as_bytes()), &data(&["cat", "artist:foo", "dog"]), &ns).unwrap();
    let text = s(&out);
    assert!(text.contains("<rdf:li>Places|Europe|France</rdf:li>"), "{text}");
    assert!(text.contains("<rdf:li>Places</rdf:li>"));
    assert_eq!(read(&out, ns).unwrap().tags, ["artist:foo", "cat", "dog"].iter().map(|t| t.to_string()).collect());
}

#[test]
fn reads_attribute_form_and_other_prefixes() {
    let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><r:RDF xmlns:r="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<r:Description r:about="" xmlns:b="urn:boorucraft:xmp:1/" xmlns:d="http://purl.org/dc/elements/1.1/" b:Rating="e" b:Source="https://x.test/&#49;">
<d:subject><r:Bag><r:li>cat</r:li></r:Bag></d:subject></r:Description></r:RDF></x:xmpmeta>"#;
    let back = read(xml.as_bytes(), ns).unwrap();
    assert_eq!(back.rating, Some(Rating::Explicit));
    assert_eq!(back.source.as_deref(), Some("https://x.test/1"));
    assert_eq!(back.tags, ["cat".to_string()].into());

    let out = update(Some(xml.as_bytes()), &data(&["dog"]), &ns).unwrap();
    let text = s(&out);
    assert!(!text.contains("b:Rating"), "{text}");
    assert!(text.contains("<r:Description r:about=\"\" xmlns:dc="), "uses the document's RDF prefix: {text}");
    assert_eq!(read(&out, ns).unwrap().tags, ["dog".to_string()].into());
}

#[test]
fn hostile_input_is_an_error_not_a_crash() {
    assert!(matches!(read(b"<a><b></a>", ns), Err(XmpError::Xml(_))));
    assert!(matches!(read(b"<a>", ns), Err(XmpError::Xml(_))));
    assert!(matches!(read(&[0xff, 0xfe, 0x00], ns), Err(XmpError::NotUtf8)));
    let deep = "<a>".repeat(MAX_DEPTH + 1);
    assert!(matches!(read(deep.as_bytes(), ns), Err(XmpError::TooDeep)));
    assert!(matches!(update(Some(b"<a/>"), &data(&["cat"]), &ns), Err(XmpError::NoRdf)));
    let huge = vec![b' '; MAX_SIDECAR_BYTES + 1];
    assert!(matches!(read(&huge, ns), Err(XmpError::TooLarge)));
    // Garbage that is valid XML reads as "nothing found".
    assert_eq!(read(b"<?xml version=\"1.0\"?><zzz>&#1114111;&unknown;</zzz>", ns).unwrap(), SidecarData::default());
}

/// darktable keeps its own keyword tree (`darktable|format|jpg`) in the shared fields.
#[test]
fn darktable_keywords_are_kept_but_are_not_tags() {
    let xml = DARKTABLE.replace(
        "   <dc:subject>\n    <rdf:Bag>\n     <rdf:li>old_tag</rdf:li>\n    </rdf:Bag>\n   </dc:subject>",
        "   <dc:subject>\n    <rdf:Bag>\n     <rdf:li>darktable</rdf:li>\n     <rdf:li>format</rdf:li>\n     <rdf:li>jpg</rdf:li>\n    </rdf:Bag>\n   </dc:subject>\n   <lr:hierarchicalSubject xmlns:lr=\"http://ns.adobe.com/lightroom/1.0/\">\n    <rdf:Bag>\n     <rdf:li>darktable|format|jpg</rdf:li>\n    </rdf:Bag>\n   </lr:hierarchicalSubject>",
    );
    assert!(xml.contains("darktable|format|jpg"));
    assert!(read(xml.as_bytes(), ns).unwrap().tags.is_empty());

    let out = update(Some(xml.as_bytes()), &data(&["cat"]), &ns).unwrap();
    let text = s(&out);
    assert!(text.contains("<rdf:li>darktable|format|jpg</rdf:li>"), "{text}");
    for kw in ["darktable", "format", "jpg", "cat"] {
        assert!(text.contains(&format!("<rdf:li>{kw}</rdf:li>")), "{kw} missing: {text}");
    }
    assert_eq!(read(&out, ns).unwrap().tags, ["cat".to_string()].into());

    // Clearing BooruCraft's data keeps darktable's keywords.
    let cleared = update(Some(&out), &SidecarData::default(), &ns).unwrap();
    let text = s(&cleared);
    assert!(text.contains("darktable|format|jpg") && !text.contains("<rdf:li>cat</rdf:li>"), "{text}");
    let again = update(Some(&cleared), &SidecarData::default(), &ns).unwrap();
    assert_eq!(s(&cleared), s(&again));
}
