use super::*;
use crate::model::*;

fn valid() -> Metadata {
    metadata(now(), now() + HOUR)
}

#[test]
fn accepts_ordinary_metadata() {
    valid().validate(now()).unwrap();
}

#[test]
fn rejects_titles_outside_youtube_limits() {
    for title in ["", "   ", &"a".repeat(101), "a <b> c"] {
        let m = Metadata {
            title: title.into(),
            ..valid()
        };
        assert!(m.validate(now()).is_err(), "accepted title {title:?}");
    }
    let m = Metadata {
        title: "a".repeat(100),
        ..valid()
    };
    m.validate(now()).unwrap();
}

#[test]
fn rejects_oversized_or_marked_up_descriptions() {
    for description in ["x".repeat(5001), "<script>".into()] {
        let m = Metadata {
            description,
            ..valid()
        };
        assert!(m.validate(now()).is_err());
    }
}

#[test]
fn rejects_empty_tags_and_overlong_tag_lists() {
    let m = Metadata {
        tags: vec!["ok".into(), "  ".into()],
        ..valid()
    };
    assert!(m.validate(now()).is_err());
    let m = Metadata {
        tags: vec!["x".repeat(501)],
        ..valid()
    };
    assert!(m.validate(now()).is_err());
    // Quoted tags count their quotes, so spaced tags reach the 500 limit sooner.
    let spaced = Metadata {
        tags: vec!["a b".to_string(); 84],
        ..valid()
    };
    assert!(spaced.validate(now()).is_err());
    let plain = Metadata {
        tags: vec!["ab".to_string(); 84],
        ..valid()
    };
    plain.validate(now()).unwrap();
}

#[test]
fn rejects_non_numeric_categories() {
    for category in ["", "gaming", "0"] {
        let m = Metadata {
            category: category.into(),
            ..valid()
        };
        assert!(m.validate(now()).is_err());
    }
}

#[test]
fn publication_must_clear_both_now_and_the_upload_time() {
    let t = now();
    assert!(metadata(t, t + 30.0).validate(t).is_err());
    assert!(metadata(t + HOUR, t + HOUR).validate(t).is_err());
    assert!(metadata(t + HOUR, t + HOUR + 30.0).validate(t).is_err());
    metadata(t + HOUR, t + HOUR + 61.0).validate(t).unwrap();
    assert!(metadata(t, f64::NAN).validate(t).is_err());
    assert!(metadata(t, f64::INFINITY).validate(t).is_err());
}

#[test]
fn fingerprint_reports_size_and_digest_and_rejects_empty_files() {
    let f = Fixture::new();
    let path = f.video("sample.mp4", 4096);
    let (size, digest) = fingerprint(&path).unwrap();
    assert_eq!(size, 4096);
    assert_eq!(digest.len(), 64);
    std::fs::write(&path, b"different").unwrap();
    assert_ne!(fingerprint(&path).unwrap().1, digest);
    std::fs::write(&path, b"").unwrap();
    assert!(fingerprint(&path).is_err());
}

#[test]
fn times_round_trip_through_rfc3339() {
    let t = (now() * 1000.0).round() / 1000.0;
    assert!((parse_time(&iso(t)).unwrap() - t).abs() < 1.0);
    assert_eq!(parse_time("2030-01-01T00:00:00Z").unwrap(), 1893456000.0);
    // An offset is required: a bare local timestamp is ambiguous.
    assert!(parse_time("2030-01-01T00:00:00").is_err());
    assert_eq!(
        parse_time("2030-01-01T01:00:00+01:00").unwrap(),
        parse_time("2030-01-01T00:00:00Z").unwrap()
    );
}

#[test]
fn the_initial_upload_is_private_and_carries_no_publication_time() {
    let f = Fixture::new();
    let body = upload_body(&f.job("sample.mp4", 512));
    assert_eq!(body["status"]["privacyStatus"], "private");
    assert!(body["status"].get("publishAt").is_none());
    assert_eq!(body["status"]["containsSyntheticMedia"], true);
    assert_eq!(body["status"]["selfDeclaredMadeForKids"], false);
    assert_eq!(body["snippet"]["title"], "A demo");
    assert_eq!(body["snippet"]["categoryId"], "22");
}
