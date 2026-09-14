use tvmatch::srt::{Cue, MAX_CUES, MAX_TIMESTAMP_MS, ParseErrorKind, Transcript};
fn cue(start_ms: u64, end_ms: u64, text: &str) -> Cue {
    Cue {
        start_ms,
        end_ms,
        text: text.into(),
    }
}
#[test]
fn checked_cue_constructor_preserves_text_and_bounds_matcher_arithmetic() {
    let t = Transcript::from_cues(vec![cue(1, 2, "Étoile\n東京\tпривет")]).unwrap();
    assert_eq!(t.cues()[0].text, "Étoile\n東京\tпривет");
    for cues in [
        vec![cue(1, 1, "a")],
        vec![cue(2, 1, "a")],
        vec![cue(0, u64::MAX, "a")],
        vec![cue(0, MAX_TIMESTAMP_MS + 1, "a")],
        vec![cue(2, 3, "a"), cue(1, 2, "b")],
    ] {
        assert_eq!(
            Transcript::from_cues(cues).unwrap_err().kind,
            ParseErrorKind::InvalidTiming
        );
    }
}
#[test]
fn checked_cue_constructor_validates_before_trimming_and_bounds_count() {
    assert_eq!(
        Transcript::from_cues(vec![]).unwrap_err().kind,
        ParseErrorKind::Empty
    );
    for text in ["\x0b", "\x0c", "text\0", "a\rb"] {
        assert_eq!(
            Transcript::from_cues(vec![cue(0, 1, text)])
                .unwrap_err()
                .kind,
            ParseErrorKind::InvalidText
        );
    }
    assert_eq!(
        Transcript::from_cues(vec![cue(0, 1, " \t\n")])
            .unwrap_err()
            .kind,
        ParseErrorKind::MissingText
    );
    assert_eq!(
        Transcript::from_cues(vec![cue(0, 1, "a"); MAX_CUES + 1])
            .unwrap_err()
            .kind,
        ParseErrorKind::LimitExceeded
    );
}
