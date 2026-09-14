use std::{
    error::Error,
    io::{Read, Seek, Write},
    ops::ControlFlow,
};
use tvmatch::{
    Index, MatchOutcome,
    media::{ocr::LocalOcr, pgs::ScanCompletion},
};
const CHECKPOINTS: [usize; 3] = [64, 128, 192];
fn sample_step(
    frames: usize,
    outcome: Option<&MatchOutcome>,
    output: &mut impl Write,
) -> std::io::Result<ControlFlow<()>> {
    if matches!(outcome, Some(MatchOutcome::Identified { .. })) {
        return Ok(ControlFlow::Break(()));
    }
    if let Some(next) = CHECKPOINTS.iter().find(|&&n| n > frames) {
        writeln!(
            output,
            "  Widening subtitle search: {frames} → {next} images."
        )?;
        Ok(ControlFlow::Continue(()))
    } else {
        Ok(ControlFlow::Break(()))
    }
}
pub(super) fn match_pgs<R: Read + Seek>(
    reader: R,
    track: u64,
    index: &Index,
    engine: &LocalOcr,
    limits: media_mkv_webm::streaming::StreamingLimits,
    output: &mut impl Write,
) -> Result<MatchOutcome, Box<dyn Error>> {
    let mut assessed = None;
    let mut failure = None;
    let ocr = tvmatch::media::ocr::extract_pgs_ocr_progressive_with_limits(
        reader,
        track,
        engine,
        &CHECKPOINTS,
        limits,
        |frames, cues| {
            let result = (|| -> Result<ControlFlow<()>, Box<dyn Error>> {
                let outcome = if cues.is_empty() {
                    None
                } else {
                    let query = tvmatch::srt::Transcript::from_cues(cues.to_vec())?;
                    Some(index.match_query_with_limits(&query, tvmatch::EPISODE_PAIR_LIMITS)?)
                };
                let step = sample_step(frames, outcome.as_ref(), output)?;
                if let Some(outcome) = outcome {
                    assessed = Some((frames, outcome));
                }
                Ok(step)
            })();
            match result {
                Ok(step) => step,
                Err(error) => {
                    failure = Some(error);
                    ControlFlow::Break(())
                }
            }
        },
    )?;
    // A callback outcome is provisional until its packet and extraction validate.
    if let Some(error) = failure {
        return Err(error);
    }
    let outcome = final_assessment(index, &ocr.transcript, ocr.frames_recognized, assessed)?;
    if !matches!(outcome, MatchOutcome::Identified { .. }) {
        let end = match ocr.scan.completion {
            ScanCompletion::Complete => "end of track",
            ScanCompletion::Stopped => "sample limit; unread suffix unvalidated",
        };
        writeln!(
            output,
            "  No confident match after {} subtitle images ({end}).",
            ocr.frames_recognized
        )?;
    }
    Ok(outcome)
}
fn final_assessment(
    index: &Index,
    transcript: &tvmatch::srt::Transcript,
    frames: usize,
    assessed: Option<(usize, MatchOutcome)>,
) -> Result<MatchOutcome, tvmatch::EngineError> {
    match assessed {
        Some((at, outcome)) if at == frames => Ok(outcome),
        _ => index.match_query_with_limits(transcript, tvmatch::EPISODE_PAIR_LIMITS),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn widening_announces_each_transition_and_has_a_hard_stop() {
        let unknown = MatchOutcome::Unknown {
            candidates: vec![],
            reasons: vec![tvmatch::RejectionReason::NoDistinctiveOverlap],
        };
        let ambiguous = MatchOutcome::Ambiguous {
            candidates: vec![],
            reason: tvmatch::RejectionReason::CompetingEvidence,
        };
        for outcome in [None, Some(&unknown), Some(&ambiguous)] {
            let mut out = Vec::new();
            assert!(sample_step(64, outcome, &mut out).unwrap().is_continue());
            assert!(sample_step(128, outcome, &mut out).unwrap().is_continue());
            assert!(sample_step(192, outcome, &mut out).unwrap().is_break());
            let text = String::from_utf8(out).unwrap();
            assert_eq!(text.matches("Widening").count(), 2);
            assert!(text.contains("64 → 128"));
            assert!(text.contains("128 → 192"));
        }
    }
    #[test]
    fn identified_stops_without_widening_or_output() {
        let transcript = tvmatch::srt::Transcript::from_cues(
            (0..3)
                .map(|i| tvmatch::srt::Cue {
                    start_ms: i * 10_000,
                    end_ms: i * 10_000 + 1000,
                    text: format!("original{i} synthetic{i} distinctive{i} phrase{i}"),
                })
                .collect(),
        )
        .unwrap();
        let reference = tvmatch::Reference::new(
            tvmatch::ReferenceId::new("test", "episode").unwrap(),
            "test episode",
            "synthetic",
            transcript.clone(),
        )
        .unwrap();
        let outcome = Index::build(vec![reference])
            .unwrap()
            .match_query(&transcript)
            .unwrap();
        assert!(matches!(outcome, MatchOutcome::Identified { .. }));
        let reference = tvmatch::Reference::new(
            tvmatch::ReferenceId::new("test", "episode").unwrap(),
            "test episode",
            "synthetic",
            transcript.clone(),
        )
        .unwrap();
        let index = Index::build(vec![reference]).unwrap();
        let earlier = tvmatch::srt::Transcript::from_cues(transcript.cues()[..2].to_vec()).unwrap();
        let unknown = index.match_query(&earlier).unwrap();
        assert!(matches!(unknown, MatchOutcome::Unknown { .. }));
        assert!(matches!(
            final_assessment(&index, &transcript, 3, Some((2, unknown))).unwrap(),
            MatchOutcome::Identified { .. }
        ));
        let mut out = Vec::new();
        assert!(
            sample_step(64, Some(&outcome), &mut out)
                .unwrap()
                .is_break()
        );
        assert!(out.is_empty());
    }
}
