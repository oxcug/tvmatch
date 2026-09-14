use std::io::{self, Read, Write};
use tvmatch::opensubtitles::{Failure, ShowInteraction, ShowOffer};

pub(super) struct ShowPicker {
    pub dry_run: bool,
}
impl ShowInteraction for ShowPicker {
    fn offer(&mut self, offer: &ShowOffer) -> Result<(), Failure> {
        display(&mut io::stdout().lock(), offer, self.dry_run)
            .map_err(|_| Failure("show choices output failed; no selection".into()))
    }
    fn select(&mut self, offer: &ShowOffer) -> Result<Option<usize>, Failure> {
        if self.dry_run {
            return Ok(None);
        }
        let mut out = io::stdout().lock();
        write!(
            out,
            "Select show (1-{}, Enter cancels): ",
            offer.candidates.len()
        )
        .and_then(|_| out.flush())
        .map_err(|_| Failure("show prompt failed; no selection".into()))?;
        Ok(read_choice(&mut io::stdin().lock(), offer.candidates.len()))
    }
}
fn display(out: &mut impl Write, offer: &ShowOffer, dry_run: bool) -> io::Result<()> {
    writeln!(
        out,
        "Show lookup unresolved for {:?}.",
        crate::rename::text(&offer.query)
    )?;
    for (i, candidate) in offer.candidates.iter().enumerate() {
        let year = candidate
            .year
            .map_or_else(|| "year unknown".into(), |n| n.to_string());
        let imdb = candidate
            .imdb_id
            .map_or_else(|| "IMDb unavailable".into(), |n| format!("tt{n:07}"));
        writeln!(
            out,
            "  {}. {} ({year}) — {imdb} [provider ID {}]",
            i + 1,
            crate::rename::text(&candidate.title),
            candidate.show_id
        )?;
    }
    if offer.truncated {
        writeln!(
            out,
            "List truncated: at most 5 choices shown; search is bounded to 10 pages / 1000 results. Other shows may exist."
        )?;
    }
    writeln!(
        out,
        "Use --imdb ttID instead to select a specific show, or select a numbered option."
    )?;
    if dry_run {
        writeln!(
            out,
            "Dry-run: choices only; no stdin read. Show remains unresolved; rerun with --imdb ttID."
        )?;
    }
    out.flush()
}
fn read_choice(input: &mut impl Read, count: usize) -> Option<usize> {
    let mut line = Vec::with_capacity(64);
    for _ in 0..65 {
        let mut byte = [0];
        if input.read(&mut byte).ok()? == 0 {
            return None;
        }
        if byte[0] == b'\n' {
            let value = std::str::from_utf8(&line).ok()?.trim();
            // Exactly one displayed digit: no signs, leading zeros or partial reads.
            return (value.len() == 1 && value.as_bytes()[0].is_ascii_digit())
                .then(|| usize::from(value.as_bytes()[0] - b'0'))
                .filter(|n| *n > 0 && *n <= count.min(5));
        }
        if line.len() == 64 {
            return None;
        }
        line.push(byte[0]);
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    use tvmatch::opensubtitles::ShowCandidate;
    #[test]
    fn numbered_consent_requires_complete_bounded_valid_line() {
        for (input, expected) in [
            ("1\n", Some(1)),
            (" 5\r\n", Some(5)),
            ("\n", None),
            ("", None),
            ("2", None),
            ("0\n", None),
            ("6\n", None),
            ("yes\n", None),
            ("1x\n", None),
            ("01\n", None),
            ("+1\n", None),
        ] {
            assert_eq!(read_choice(&mut input.as_bytes(), 5), expected, "{input:?}");
        }
        assert_eq!(read_choice(&mut "2\n".as_bytes(), 1), None);
        assert_eq!(read_choice(&mut [0xff, b'\n'].as_slice(), 5), None);
        assert_eq!(
            read_choice(&mut format!("{}1\n", " ".repeat(64)).as_bytes(), 5),
            None
        );
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("synthetic"))
            }
        }
        assert_eq!(read_choice(&mut Broken, 5), None);
        let mut two_lines = "1\n5\n".as_bytes();
        assert_eq!(read_choice(&mut two_lines, 5), Some(1));
        assert_eq!(two_lines, b"5\n");
    }
    #[test]
    fn choices_include_identity_guidance_truncation_and_dry_run_bypasses_stdin() {
        let offer = ShowOffer {
            query: "Original".into(),
            candidates: vec![
                ShowCandidate {
                    show_id: 1,
                    title: "Original Show".into(),
                    year: Some(2012),
                    imdb_id: Some(123),
                },
                ShowCandidate {
                    show_id: 2,
                    title: "Original Show".into(),
                    year: None,
                    imdb_id: None,
                },
            ],
            truncated: true,
        };
        let mut bytes = Vec::new();
        display(&mut bytes, &offer, true).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        for expected in [
            "1. Original Show (2012)",
            "tt0000123",
            "2. Original Show (year unknown)",
            "IMDb unavailable",
            "provider ID 2",
            "List truncated",
            "--imdb",
            "numbered option",
            "no stdin read",
        ] {
            assert!(text.contains(expected), "{expected}");
        }
        assert_eq!(ShowPicker { dry_run: true }.select(&offer).unwrap(), None);
    }
}
