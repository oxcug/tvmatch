use std::{collections::BTreeMap, error::Error, ffi::OsString, path::PathBuf};
#[allow(dead_code)] // Minimal feature builds validate the same interface before reporting unavailable.
pub(super) struct Options {
    pub folder: PathBuf,
    pub show: Option<String>,
    pub imdb: Option<String>,
    pub season: String,
    pub episodes: Option<String>,
    pub dry_run: bool,
}
pub(super) fn parse(args: &[OsString]) -> Result<Options, Box<dyn Error>> {
    if args.len() > 11 || args.iter().any(|a| a.len() > 4096) {
        return Err("argument limit exceeded".into());
    }
    let mut values = BTreeMap::new();
    let mut dry_run = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let key = arg.to_str().ok_or("flag must be UTF-8")?;
        if key == "--dry-run" {
            if dry_run {
                return Err("duplicate --dry-run".into());
            }
            dry_run = true;
            continue;
        }
        if !matches!(
            key,
            "--folder" | "--show" | "--imdb" | "--season" | "--episodes"
        ) {
            return Err("invalid or duplicate arguments (see --help)".into());
        }
        let value = args
            .next()
            .ok_or("incomplete arguments: missing value (see --help)")?;
        if value.to_str().is_some_and(|s| s.starts_with("--"))
            || values.insert(key, value).is_some()
        {
            return Err("invalid, duplicate or incomplete arguments (see --help)".into());
        }
    }
    let text = |key| -> Result<Option<&str>, Box<dyn Error>> {
        values
            .get(key)
            .map(|v| v.to_str().ok_or_else(|| "metadata must be UTF-8".into()))
            .transpose()
    };
    let show = text("--show")?;
    let imdb = text("--imdb")?;
    if show.is_none() && imdb.is_none() {
        return Err("provide --show or --imdb (both allow a display-name override)".into());
    }
    if show.is_some_and(|s| {
        s.len() < if imdb.is_some() { 1 } else { 3 }
            || s.len() > 200
            || s.trim() != s
            || s.chars().any(char::is_control)
    }) {
        return Err(
            "show must be explicit UTF-8 metadata (3..200 bytes for lookup, 1..200 for override)"
                .into(),
        );
    }
    if let Some(id) = imdb {
        tvmatch::imdb_number(id)?;
    }
    let season = text("--season")?.ok_or("--season is required")?;
    number(season, 100)?;
    let episodes = text("--episodes")?;
    if let Some(range) = episodes {
        let (a, b) = range.split_once('-').unwrap_or((range, range));
        let (a, b) = (number(a, 1000)?, number(b, 1000)?);
        if b < a || b - a >= 1000 {
            return Err("episode range must be ordered, episode numbers up to 1000".into());
        }
    }
    let default_folder = OsString::from(".");
    let folder = values.get("--folder").copied().unwrap_or(&default_folder);
    if folder.is_empty() {
        return Err("folder path must not be empty".into());
    }
    Ok(Options {
        folder: PathBuf::from(folder),
        show: show.map(str::to_owned),
        imdb: imdb.map(str::to_owned),
        season: season.into(),
        episodes: episodes.map(str::to_owned),
        dry_run,
    })
}
fn number(s: &str, max: u64) -> Result<u64, Box<dyn Error>> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err("expected positive bounded decimal number".into());
    }
    s.parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && *n <= max)
        .ok_or_else(|| "positive decimal number outside bounds".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imdb_accepts_display_override_without_changing_selector_and_full_argument_budget() {
        let args = [
            "--folder",
            "folder",
            "--imdb",
            "tt1695360",
            "--show",
            "K",
            "--season",
            "3",
            "--episodes",
            "1-13",
            "--dry-run",
        ]
        .map(OsString::from);
        let parsed = parse(&args).unwrap();
        assert_eq!(parsed.imdb.as_deref(), Some("tt1695360"));
        assert_eq!(parsed.show.as_deref(), Some("K"));
        assert!(parsed.dry_run);
        for name in ["", " bad", "bad\u{0007}"] {
            let args = ["--imdb", "tt1695360", "--show", name, "--season", "3"].map(OsString::from);
            assert!(parse(&args).is_err());
        }
        assert!(parse(&["--show", "K", "--season", "3"].map(OsString::from)).is_err());
    }
    #[test]
    fn dry_run_is_valueless_optional_and_not_repeatable() {
        let base = [
            "--imdb",
            "tt0343314",
            "--season",
            "2",
            "--folder",
            "./media/Example Show/Season 2",
            "--episodes",
            "1-13",
        ]
        .map(OsString::from)
        .to_vec();
        assert!(!parse(&base).unwrap().dry_run);
        for at in [0, 2, 4, 6, 8] {
            let mut args = base.clone();
            args.insert(at, "--dry-run".into());
            assert!(parse(&args).unwrap().dry_run);
            args.push("--dry-run".into());
            assert!(parse(&args).is_err());
        }
        let mut args = base;
        args.extend(["--dry-run", "yes"].map(OsString::from));
        assert!(parse(&args).is_err());
    }
}
