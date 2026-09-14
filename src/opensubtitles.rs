//! Bounded independent reference acquisition. No media-derived request data.
mod boundaries;
mod cache;
mod catalog;
mod fallback;
mod show_selection;
pub use fallback::{FallbackInteraction, FallbackOffer};
pub use show_selection::{ShowCandidate, ShowInteraction, ShowOffer};
mod ordering;
mod records;
mod series;
#[cfg(test)]
mod tests;
mod transport;
use crate::{Reference, ReferenceId, srt::Transcript};
pub use cache::Cache;
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};
pub use transport::Online;

/// Provider episode-number/metadata guard, independent of account quota.
pub const MAX_EPISODES: u32 = 1000;
const POLICY: u32 = 1;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure(pub String);
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl Error for Failure {}
type Result<T> = std::result::Result<T, Failure>;
fn fail(message: &str) -> Failure {
    Failure(message.into())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub show: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb: Option<String>,
    pub season: u32,
    pub episodes: Option<(u32, u32)>,
}
impl Scope {
    pub fn new(show: &str, season: &str, episodes: Option<&str>) -> Result<Self> {
        if show.trim() != show
            || show.len() < 3
            || show.len() > 200
            || show.chars().any(char::is_control)
        {
            return Err(fail(
                "show must be explicit, nonempty UTF-8 metadata (3..200 bytes)",
            ));
        }
        let season = positive(season, 100)?;
        let episodes = episodes
            .map(|range| {
                let (a, b) = range.split_once('-').unwrap_or((range, range));
                let a = positive(a, 1000)?;
                let b = positive(b, 1000)?;
                if b < a || b - a + 1 > MAX_EPISODES {
                    return Err(fail(
                        "episode range must be inclusive, ordered, episode numbers up to 1000",
                    ));
                }
                Ok((a, b))
            })
            .transpose()?;
        Ok(Self {
            show: show.into(),
            imdb: None,
            season,
            episodes,
        })
    }
    pub fn from_imdb(imdb: &str, season: &str, episodes: Option<&str>) -> Result<Self> {
        crate::imdb_number(imdb).map_err(|_| fail("invalid bounded IMDb ttID"))?;
        let mut scope = Self::new("IMDb", season, episodes)?;
        scope.show.clear();
        scope.imdb = Some(imdb.into());
        Ok(scope)
    }
    fn validate(&self) -> Result<()> {
        let range = self.episodes.map(|(a, b)| format!("{a}-{b}"));
        if let Some(imdb) = &self.imdb {
            if !self.show.is_empty() {
                return Err(fail("scope cannot contain both name and IMDb"));
            }
            Self::from_imdb(imdb, &self.season.to_string(), range.as_deref())?;
        } else {
            Self::new(&self.show, &self.season.to_string(), range.as_deref())?;
        }
        Ok(())
    }
}
fn positive(text: &str, max: u32) -> Result<u32> {
    if text.is_empty() || !text.bytes().all(|c| c.is_ascii_digit()) {
        return Err(fail("expected positive bounded decimal number"));
    }
    text.parse::<u32>()
        .ok()
        .filter(|n| (1..=max).contains(n))
        .ok_or_else(|| fail("positive decimal number outside bounds"))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Selected {
    pub show_id: u64,
    pub episode_id: u64,
    pub season: u32,
    pub episode: u32,
    pub title: String,
    pub subtitle_id: u64,
    pub file_id: u64,
    pub language: String,
    pub release: String,
    pub uploader: String,
    pub source: String,
}
impl Selected {
    pub fn label(&self) -> String {
        format!("S{:02}E{:02} {}", self.season, self.episode, self.title)
    }
    fn validate(&self, show_id: u64, season: u32) -> Result<()> {
        if self.show_id != show_id
            || self.season != season
            || self.episode == 0
            || self.episode > 1000
            || self.language != "en"
            || [
                self.show_id,
                self.episode_id,
                self.subtitle_id,
                self.file_id,
            ]
            .iter()
            .any(|id| *id == 0 || *id > i32::MAX as u64)
            || !catalog::clean(&self.title, 512)
            || !catalog::clean_optional(&self.release, 1024)
            || !catalog::clean_optional(&self.uploader, 200)
            || self.source
                != format!(
                    "https://www.opensubtitles.com/en/subtitles/{}",
                    self.subtitle_id
                )
        {
            return Err(fail("invalid/conflicting cached reference identity"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum UnavailableReason {
    #[serde(rename = "no_eligible_english_reference_v1")]
    NoEligibleEnglishReference,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnavailableVariant {
    episode: u32,
    episode_id: u64,
    title: String,
    reason: UnavailableReason,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    policy: u32,
    scope: Scope,
    pub show_id: u64,
    pub show_title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    show_choice: Option<show_selection::Choice>,
    pub selected: Vec<Selected>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unavailable: Vec<UnavailableVariant>,
}
impl Manifest {
    fn report_unavailable(&self) {
        for variant in &self.unavailable {
            eprintln!(
                "Warning: S{:02}E{:02} catalog ID {} was unavailable at selection (no eligible English reference); other available IDs cover this episode. IDs are not merged.",
                self.scope.season, variant.episode, variant.episode_id
            );
        }
    }
    fn validate(&self, scope: &Scope) -> Result<()> {
        scope.validate()?;
        if self.policy != POLICY
            || &self.scope != scope
            || self.selected.is_empty()
            || self.selected.len().saturating_add(self.unavailable.len()) > MAX_EPISODES as usize
            || !catalog::clean(&self.show_title, 200)
            || (scope.imdb.is_none()
                && !self.show_title.eq_ignore_ascii_case(&scope.show)
                && self.show_choice.is_none())
        {
            return Err(fail("cache manifest scope/policy mismatch"));
        }
        if let Some(choice) = &self.show_choice {
            choice.matches(self)?;
        }
        let mut files = std::collections::BTreeSet::new();
        let mut ids = std::collections::BTreeSet::new();
        let mut episodes = std::collections::BTreeSet::new();
        for selected in &self.selected {
            selected.validate(self.show_id, scope.season)?;
            if !files.insert(selected.file_id) || !ids.insert(selected.episode_id) {
                return Err(fail("duplicate reference file/episode identity"));
            }
            episodes.insert(selected.episode);
        }
        for variant in &self.unavailable {
            if variant.episode_id == 0
                || variant.episode_id > i32::MAX as u64
                || !ids.insert(variant.episode_id)
                || !episodes.contains(&variant.episode)
                || !catalog::clean(&variant.title, 512)
            {
                return Err(fail(
                    "invalid unavailable catalog variant or uncovered episode",
                ));
            }
        }
        if !self
            .unavailable
            .windows(2)
            .all(|w| (w[0].episode, w[0].episode_id) < (w[1].episode, w[1].episode_id))
        {
            return Err(fail("unordered unavailable catalog variants"));
        }
        if let Some((a, b)) = scope.episodes
            && episodes != (a..=b).collect()
        {
            return Err(fail("incomplete episode range in manifest"));
        }
        if !self
            .selected
            .windows(2)
            .all(|w| (w[0].episode, w[0].episode_id) < (w[1].episode, w[1].episode_id))
        {
            return Err(fail("unordered episode manifest"));
        }
        Ok(())
    }
}

/// A complete verified cache returns before constructing Online or reading credentials.
pub fn references(cache: &Cache, scope: &Scope, fetch: bool) -> Result<Vec<Reference>> {
    references_inner(cache, scope, fetch, None, None, None)
}
/// Rename-ready references and a provider original-title/year prefix. A supplied
/// name overrides display only, never the scope/identity or frozen selection.
/// Legacy caches may need one metadata GET, but never a subtitle re-download.
pub fn references_for_rename(
    cache: &Cache,
    scope: &Scope,
    fetch: bool,
    name_override: Option<&str>,
) -> Result<(Vec<Reference>, String)> {
    series::valid_override(name_override)?;
    let refs = references_inner(cache, scope, fetch, Some(name_override), None, None)?;
    Ok((refs, cache.series_prefix(scope, name_override)?))
}
/// Rename-ready references with an explicit caller-owned empty-reference fallback UI.
/// At most one fallback POST per call; dry-run offers metadata but never confirms.
/// Normal missing-reference acquisition still follows `fetch`, independently of dry-run.
pub fn references_for_rename_with_fallback(
    cache: &Cache,
    scope: &Scope,
    fetch: bool,
    name_override: Option<&str>,
    dry_run: bool,
    interaction: &mut impl FallbackInteraction,
) -> Result<(Vec<Reference>, String)> {
    series::valid_override(name_override)?;
    let mut options = fallback::Options {
        dry_run,
        interaction,
    };
    let refs = references_inner(
        cache,
        scope,
        fetch,
        Some(name_override),
        Some(&mut options),
        None,
    )?;
    Ok((refs, cache.series_prefix(scope, name_override)?))
}
/// Rename-ready references with caller-owned show selection and empty-reference
/// fallback UIs. Existing frozen scopes and explicit IMDb bypass show selection.
/// Dry-run displays unresolved choices but never calls either consent callback.
pub fn references_for_rename_with_interactions(
    cache: &Cache,
    scope: &Scope,
    fetch: bool,
    name_override: Option<&str>,
    dry_run: bool,
    shows: &mut impl ShowInteraction,
    fallback: &mut impl FallbackInteraction,
) -> Result<(Vec<Reference>, String)> {
    series::valid_override(name_override)?;
    let mut show_options = show_selection::Options {
        dry_run,
        interaction: shows,
    };
    let mut fallback_options = fallback::Options {
        dry_run,
        interaction: fallback,
    };
    let refs = references_inner(
        cache,
        scope,
        fetch,
        Some(name_override),
        Some(&mut fallback_options),
        Some(&mut show_options),
    )?;
    Ok((refs, cache.series_prefix(scope, name_override)?))
}
// None: reference-only API. Some(None): default display. Some(Some): override.
fn references_inner(
    cache: &Cache,
    scope: &Scope,
    fetch: bool,
    display: Option<Option<&str>>,
    fallback: Option<&mut fallback::Options<'_>>,
    shows: Option<&mut show_selection::Options<'_>>,
) -> Result<Vec<Reference>> {
    scope.validate()?;
    let base = cache.manifest(scope)?;
    let existing = base
        .as_ref()
        .map(|m| cache.effective_manifest(m))
        .transpose()?;
    if let Some(manifest) = &existing {
        manifest.report_unavailable();
        let (pending, failures) = recover_missing(cache, manifest)?;
        if pending.is_empty() && !failures.is_empty() && (fallback.is_none() || !fetch) {
            return Err(incomplete(&failures));
        }
        let missing = cache.missing(manifest)?;
        if missing.is_empty() {
            if let Some(name) = display {
                let d = match cache.display(manifest)? {
                    Some(d) => d,
                    None if fetch => {
                        let mut online=Online::from_env().map_err(|e|fail(&format!("series original title/year not cached; metadata-only lookup unavailable: {e}")))?;
                        series::ensure(cache, manifest, &mut online)?
                    }
                    None => {
                        return Err(fail(
                            "series original title/year not cached; metadata acquisition disabled",
                        ));
                    }
                };
                d.prefix(name)?;
            }
            println!(
                "Using {} cached reference subtitles (no downloads).",
                manifest.selected.len()
            );
            return cache.references(manifest);
        }
        println!(
            "Reference cache: {} reference subtitles need acquisition or recovery.",
            missing.len()
        );
    } else {
        println!("Looking up the episode catalog.");
    }
    if !fetch {
        return Err(fail(
            "offline reference coverage incomplete; acquisition disabled by library caller",
        ));
    }
    let mut online = Online::from_env()?;
    let base = match base {
        Some(manifest) => manifest,
        None => resolve_manifest(cache, scope, &mut online, shows)?,
    };
    let manifest = cache.effective_manifest(&base)?;
    if let Some(name) = display {
        series::ensure(cache, &manifest, &mut online)?.prefix(name)?;
    }
    let effective = acquire_with_fallback(cache, &base, &mut online, fallback)?;
    cache.references(&effective)
}

fn resolve_manifest<T: transport::Transport>(
    cache: &Cache,
    scope: &Scope,
    online: &mut T,
    shows: Option<&mut show_selection::Options<'_>>,
) -> Result<Manifest> {
    // Recheck the frozen request before offering any alternative identity.
    if let Some(manifest) = cache.manifest(scope)? {
        return Ok(manifest);
    }
    // Cache operations above have released their shared IO transactions. No
    // global transaction or new season lock spans caller-owned show prompting.
    let resolved = catalog::resolve_show_with_selection(online, scope, shows)?;
    cache.lock_season(resolved.id, scope.season)?;
    let manifest = match cache.alias_with_choice(
        scope,
        resolved.id,
        &resolved.title,
        resolved.choice.clone(),
    )? {
        Some(manifest) => manifest,
        None => catalog::resolve_season_with_choice(
            online,
            scope,
            resolved.id,
            resolved.title,
            Some(cache),
            resolved.choice,
        )?,
    };
    manifest.validate(scope)?;
    cache.freeze(&manifest)?;
    manifest.report_unavailable();
    println!(
        "Episode catalog ready: {} reference subtitles selected.",
        manifest.selected.len()
    );
    Ok(manifest)
}

fn acquire_with_fallback<T: transport::Transport>(
    cache: &Cache,
    base: &Manifest,
    online: &mut T,
    fallback: Option<&mut fallback::Options<'_>>,
) -> Result<Manifest> {
    let manifest = cache.effective_manifest(base)?;
    match acquire(cache, &manifest, online) {
        Ok(()) => Ok(manifest),
        Err(error) => {
            if transport::stops_acquisition(&error) {
                return Err(error);
            }
            match fallback {
                Some(options) => fallback::attempt(cache, base, online, options),
                None => Err(error),
            }
        }
    }
}

fn acquire<T: transport::Transport>(
    cache: &Cache,
    manifest: &Manifest,
    online: &mut T,
) -> Result<()> {
    let (pending, mut failures) = recover_missing(cache, manifest)?;
    if pending.is_empty() {
        return if failures.is_empty() {
            Ok(())
        } else {
            Err(incomplete(&failures))
        };
    }
    cache.prepare(manifest)?;
    let mut remaining = online.quota()?;
    println!(
        "Download quota: {} remaining ({})",
        remaining.map_or_else(|| "not yet known".into(), |n| n.to_string()),
        if online.authenticated() {
            "account"
        } else {
            "app key"
        }
    );
    if remaining.is_some_and(|n| n < pending.len() as u64) {
        return Err(fail(
            "server quota insufficient for missing references; no download POST attempted",
        ));
    }
    let mut downloads = 0;
    let count = pending.len();
    for (index, selected) in pending.into_iter().enumerate() {
        if remaining == Some(0) {
            return Err(fail("server quota exhausted; partial coverage retained"));
        }
        // Never revisit this selected file in this invocation, even after a content failure.
        if let Err(error) = cache.reserve(selected) {
            failures.push(episode_failure(selected, &error));
            continue;
        }
        println!(
            "Downloading reference: S{:02}E{:02}",
            selected.season, selected.episode
        );
        let value = online.api(
            "download",
            Some(serde_json::json!({"file_id": selected.file_id, "sub_format": "srt"})),
        )?;
        let n = catalog::counter(&value["remaining"])?;
        remaining = Some(n); // Preserve the known allowance even if link/content/parsing fails.
        downloads += 1;
        let reset = value["reset_time_utc"]
            .as_str()
            .filter(|s| {
                s.len() <= 40
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || b"-:.TZ+".contains(&b))
            })
            .unwrap_or("unavailable");
        println!("Download quota: {n} remaining; resets {reset}.");
        let result = (|| {
            let link = value["link"]
                .as_str()
                .ok_or_else(|| fail("download response missing link; charge uncertain"))?;
            let bytes = online.content(link)?;
            cache.publish(selected, &bytes)?;
            println!(
                "Cached reference: S{:02}E{:02}",
                selected.season, selected.episode
            );
            Ok(())
        })();
        if let Err(error) = result {
            let message = episode_failure(selected, &error);
            eprintln!("{message}");
            if transport::stops_acquisition(&error) {
                return Err(fail(&message));
            }
            failures.push(message);
        }
        if n < (count - index - 1) as u64 {
            return Err(fail(
                "server remaining insufficient; any downloaded bytes/progress retained, acquisition paused without further POST",
            ));
        }
    }
    if !failures.is_empty() {
        return Err(incomplete(&failures));
    }
    println!("References ready ({downloads} downloads).");
    Ok(())
}

fn episode_failure(s: &Selected, error: &Failure) -> String {
    format!(
        "S{:02}E{:02} file_id={}: {error}",
        s.season, s.episode, s.file_id
    )
}
fn incomplete(failures: &[String]) -> Failure {
    fail(&format!(
        "reference coverage incomplete; {}",
        failures.join("; ")
    ))
}
fn recover_missing<'a>(
    cache: &Cache,
    manifest: &'a Manifest,
) -> Result<(Vec<&'a Selected>, Vec<String>)> {
    cache.pin(manifest)?;
    let mut pending = Vec::new();
    let mut failures = Vec::new();
    for s in &manifest.selected {
        match cache.recover(s) {
            Ok(true) => {}
            Ok(false) => pending.push(s),
            Err(error) => failures.push(episode_failure(s, &error)),
        }
    }
    Ok((pending, failures))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptionNormalization {
    policy: String,
    replacements: usize,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    codepoints: std::collections::BTreeMap<String, usize>,
}
struct Prepared {
    transcript: Transcript,
    caption_normalization: Option<CaptionNormalization>,
    cue_ordering: Option<ordering::CueOrdering>,
    cue_boundaries: Option<boundaries::CueBoundaries>,
    zero_duration: Option<records::ZeroDuration>,
    missing_text: Option<records::MissingText>,
    caption_paragraphs: Option<records::CaptionParagraphs>,
    source_layout: Option<records::SourceLayout>,
    cue_numbering: Option<records::CueNumbering>,
    record_framing: Option<records::RecordFraming>,
}
fn transcript(bytes: &[u8]) -> Result<Prepared> {
    if bytes.len() > crate::srt::MAX_SRT_BYTES {
        return Err(fail("reference content byte cap exceeded"));
    }
    let decoded = crate::srt::layout::decode(bytes)
        .map_err(|e| fail(&format!("reference text decoding: {e}")))?;
    let repaired = if decoded.encoding == "utf-8" {
        records::prepare(&decoded.text)?
    } else {
        records::prepare_decoded(&decoded.text, decoded.encoding)?
    };
    let (parsed, cue_ordering) = ordering::parse(&repaired.text)?;
    repaired.verify(&parsed)?;
    let records::Prepared {
        source_layout,
        zero_duration,
        missing_text,
        caption_paragraphs,
        cue_numbering,
        cue_boundaries,
        record_framing,
        caption_normalization,
        ..
    } = repaired;
    Ok(Prepared {
        source_layout,
        transcript: parsed,
        caption_normalization,
        cue_ordering,
        cue_boundaries,
        zero_duration,
        missing_text,
        caption_paragraphs,
        cue_numbering,
        record_framing,
    })
}
