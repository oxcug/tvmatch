mod cli;
#[cfg(all(feature = "opensubtitles", feature = "ocr"))]
mod folder;
#[cfg(all(feature = "opensubtitles", feature = "ocr"))]
mod rename;
use std::{error::Error, process::ExitCode};
const USAGE: &str = "tvmatch: identify MKV/MP4 files using independent English subtitle evidence
Usage: tvmatch [--folder PATH] (--show NAME | --imdb ttID [--show NAME]) --season N [--episodes N|A-B] [--dry-run]
       tvmatch --help | -h
Provide a show name or IMDb ID (tt followed by 1..10 digits, value 1..2147483647).
--imdb selects the show; optional --show overrides only its display name, not identity.
Without --imdb, --show supplies the search and display name; a unique exact match is automatic.
Otherwise choose from up to 5 numbered TV shows on stdin (title/year/IMDb when available),
or use --imdb ttID instead. Nonexact results require explicit selection, never a fuzzy guess.
Search is bounded to 10 pages / 1000 results; truncated choices are labeled, not assumed unique.
Enter, EOF, invalid, oversized or unreadable input cancels show selection.
--dry-run lists unresolved show choices and --imdb guidance, then stops without reading stdin.
Explicit IMDb and frozen cached selections are never replaced by a show prompt.
Season 1..100; episodes 1..1000. Folder defaults to current directory.
Enabled, non-forced declared-English tracks are tried one at a time,
in track-number order, stopping at the first confident match; no track-score pooling.
Formats: MKV PGS/UTF-8 and non-fragmented MP4/M4V tx3g (UTF-8 or BOM-marked UTF-16).
Text tracks are decoded without OCR and read fully within fixed budgets.
Reference SRT accepts UTF-8 or BOM-marked UTF-16; no encoding guesses or lossy decoding.
Nonrecursive, at most 32 .mkv/.mp4/.m4v files, case-insensitive.
No encrypted/fragmented MP4, burned-in subtitle recognition, sidecars, ASS/SSA,
WebVTT/TTML or non-English reference acquisition yet.
Bundled OCR starts with 64 visible frames, widening to 128 then 192 when needed.
Each widening is announced; image samples do not validate the unread suffix.
Verified reference AND series-title/year cache hits use zero HTTP/credentials.
Older caches may need one metadata-only GET for the original title and show year.
Missing references are acquired
 automatically via OpenSubtitles HTTPS (OPENSUBTITLES_API_TOKEN; optional
 OPENSUBTITLES_BEARER_TOKEN). Server quota applies; no POST retries within a run.
Entirely caption-empty references can offer ranked alternative versions.
Download fallback file ...? (y/N): requires a separate y/yes; download quota may apply.
At most one fallback download per run and three approved alternatives per original.
Original bytes and approval history are retained; malformed/weak matches do not trigger fallback.
Private reference cache cap: 250MiB; oldest eligible fetched entries evicted first.
Fallback audit files and approved versions are protected and count against this cap.
Scan once and preview renames; Apply renames? (y/N): requires y/yes on stdin.
Enter, n, EOF or other input leaves files unchanged. No second scan is needed.
--dry-run shows the preview without prompting or reading stdin; no media renames.
Normal reference acquisition/cache recovery still occurs during a dry run.
For empty references, --dry-run lists alternatives only: no fallback prompt or replacement download.
Preview includes missing episode matches for the requested season/range; unrecognized
or unsampled content may contain them. Emoji mark per-file preview states only.
Names: Original Title (Year) - S01E08 - Episode Title.mkv, from identified metadata only.
The default name is provider original_title (which may be non-English), not title case.
--show overrides the name only; the four-digit show year always comes from metadata,
not the season year. Example: --imdb tt1695360 --show \"The Legend of Korra\" --season 3
produces: The Legend of Korra (2012) - S03E06 - Old Wounds.mkv.
No model downloads, external OCR, media uploads or forced episode assignment.
Occupied destinations are allowed if their files move away in the same plan.
Chains are ordered; swaps use temporary names. No overwrites or automatic rollback.
Apply failures can leave earlier successes or reported temporary files.
Exit: 1 input/scan/conflict/apply error (takes precedence), 3 Ambiguous, 2 Unknown,
 0 all Identified with no errors (including declined preview).
Provider labels and subtitle evidence are not audio verification or calibrated confidence.";
fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}
fn run() -> Result<u8, Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).take(16).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("{USAGE}");
        return Ok(0);
    }
    let options = cli::parse(&args)?;
    #[cfg(all(feature = "opensubtitles", feature = "ocr"))]
    return folder::run(options);
    #[cfg(not(all(feature = "opensubtitles", feature = "ocr")))]
    {
        let _ = options;
        Err(
            "folder matching requires features ocr,opensubtitles; install with default features"
                .into(),
        )
    }
}
