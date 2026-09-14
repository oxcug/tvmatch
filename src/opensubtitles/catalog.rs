use super::{Manifest, POLICY, Result, Scope, Selected, fail, transport::Transport};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
const MAX_PAGES: u64 = 10;
const MAX_RESULTS: usize = 1000;
pub(super) fn clean(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && clean_optional(s, max) && s.trim() == s
}
pub(super) fn clean_optional(s: &str, max: usize) -> bool {
    s.len() <= max && !s.chars().any(char::is_control)
}
pub(super) fn counter(v: &Value) -> Result<u64> {
    v.as_u64()
        .or_else(|| {
            v.as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n <= i32::MAX as f64)
                .map(|n| n as u64)
        })
        .filter(|n| *n <= i32::MAX as u64)
        .ok_or_else(|| fail("missing/invalid bounded provider counter"))
}
pub(super) fn id(v: &Value) -> Result<u64> {
    let n = match v.as_str() {
        Some(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => s.parse().ok(),
        _ => v.as_u64(),
    };
    n.filter(|n| *n > 0 && *n <= i32::MAX as u64)
        .ok_or_else(|| fail("missing/invalid provider ID"))
}
pub(super) fn text(v: &Value, max: usize) -> Result<String> {
    v.as_str()
        .filter(|s| clean(s, max))
        .map(str::to_owned)
        .ok_or_else(|| fail("missing/invalid provider text metadata"))
}
// Provider JSON sometimes contains HTML character references in its title value.
// Decode one layer only for comparison; retain the exact catalog title in frozen
// selections. This is not markup parsing, case folding or identity reconciliation.
fn title_characters(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest.find(';').filter(|n| *n <= 12).and_then(|end| {
            let entity = &rest[1..end];
            let c = match entity {
                "amp" | "AMP" => Some('&'),
                "lt" | "LT" => Some('<'),
                "gt" | "GT" => Some('>'),
                "quot" | "QUOT" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{00a0}'),
                _ => {
                    let (digits, radix) = if let Some(s) = entity
                        .strip_prefix("#x")
                        .or_else(|| entity.strip_prefix("#X"))
                    {
                        (s, 16)
                    } else {
                        (entity.strip_prefix('#')?, 10)
                    };
                    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                        return None;
                    }
                    u32::from_str_radix(digits, radix)
                        .ok()
                        .and_then(char::from_u32)
                }
            }?;
            (!c.is_control()).then_some((end, c))
        });
        if let Some((end, c)) = decoded {
            out.push(c);
            rest = &rest[end + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}
fn titles_agree(a: &str, b: &str) -> bool {
    a == b || title_characters(a) == title_characters(b)
}
fn optional_text(v: &Value, max: usize) -> Result<String> {
    if v.is_null() {
        return Ok(String::new());
    }
    v.as_str()
        .filter(|s| clean_optional(s, max))
        .map(str::to_owned)
        .ok_or_else(|| fail("invalid optional provider text metadata"))
}
fn array(v: &Value) -> Result<&Vec<Value>> {
    v.as_array()
        .filter(|a| a.len() <= MAX_RESULTS)
        .ok_or_else(|| fail("missing/oversized provider array"))
}
pub(super) fn feature_id(v: &Value) -> Result<u64> {
    let attribute = &v["attributes"]["feature_id"];
    let identity = if attribute.is_null() {
        id(&v["id"])?
    } else {
        id(attribute)?
    };
    if !v["id"].is_null() && id(&v["id"])? != identity {
        return Err(fail("conflicting feature identity"));
    }
    Ok(identity)
}
#[cfg(test)]
pub(super) fn resolve<T: Transport>(online: &mut T, scope: &Scope) -> Result<Manifest> {
    let (id, title) = resolve_show(online, scope)?;
    resolve_season(online, scope, id, title, None)
}
#[cfg(test)]
pub(super) fn resolve_show<T: Transport>(online: &mut T, scope: &Scope) -> Result<(u64, String)> {
    let resolved = resolve_show_with_selection(online, scope, None)?;
    Ok((resolved.id, resolved.title))
}
pub(super) fn resolve_show_with_selection<T: Transport>(
    online: &mut T,
    scope: &Scope,
    options: Option<&mut super::show_selection::Options<'_>>,
) -> Result<super::show_selection::Resolution> {
    scope.validate()?;
    let query = if let Some(imdb) = &scope.imdb {
        format!(
            "features?imdb_id={}&type=tvshow",
            crate::imdb_number(imdb).map_err(|_| fail("invalid IMDb ID"))?
        )
    } else {
        format!(
            "features?query={}{}&type=tvshow",
            encode(&scope.show),
            if options.is_none() {
                "&query_match=exact"
            } else {
                ""
            }
        )
    };
    let (shows, truncated) = search_shows(online, scope, &query)?;
    if scope.imdb.is_some() {
        if truncated || shows.len() != 1 {
            return Err(fail("IMDb show resolution absent, ambiguous or truncated"));
        }
        return Ok(super::show_selection::Resolution::automatic(
            shows.into_iter().next().unwrap(),
        ));
    }
    super::show_selection::choose(scope, shows, truncated, options)
}
fn search_shows<T: Transport>(
    online: &mut T,
    scope: &Scope,
    query: &str,
) -> Result<(Vec<super::ShowCandidate>, bool)> {
    let mut shows = Vec::new();
    let mut seen = BTreeSet::new();
    let mut pages = None;
    let mut total = None;
    for page in 1..=MAX_PAGES {
        let path = if page == 1 {
            query.to_owned()
        } else {
            format!("{query}&page={page}")
        };
        let response = online.api(&path, None)?;
        let data = array(&response["data"])?;
        // The features API also has an unpaginated data-only envelope. When it
        // advertises pagination, verify progression and stable counts explicitly.
        let advertised = response.get("total_pages").map(counter).transpose()?;
        let count = response.get("total_count").map(counter).transpose()?;
        let current = response.get("page").map(counter).transpose()?;
        if current.is_some_and(|n| n != page)
            || (advertised.is_some_and(|n| n > 1) && current.is_none())
            || (page > 1 && (pages != advertised || total != count))
            || (advertised == Some(0) && (!data.is_empty() || count.is_some_and(|n| n != 0)))
            || (advertised.is_some_and(|n| n > 0 && n < page))
        {
            return Err(fail("show search pagination inconsistent"));
        }
        pages = advertised;
        total = count;
        let available = MAX_RESULTS - shows.len();
        for value in data.iter().take(available) {
            let a = &value["attributes"];
            let title = text(&a["title"], 200)?;
            let kind = a["feature_type"]
                .as_str()
                .or_else(|| a["type"].as_str())
                .or_else(|| value["type"].as_str())
                .unwrap_or("");
            if !kind.eq_ignore_ascii_case("tvshow") && !kind.eq_ignore_ascii_case("tv show") {
                return Err(fail("show response has wrong feature type"));
            }
            let show_id = feature_id(value)?;
            if !seen.insert(show_id) {
                return Err(fail("duplicate show response identity across pages"));
            }
            let imdb_id = if a["imdb_id"].is_null() {
                None
            } else {
                Some(id(&a["imdb_id"])? as u32)
            };
            if scope
                .imdb
                .as_ref()
                .is_some_and(|imdb| crate::imdb_number(imdb).ok() != imdb_id)
            {
                return Err(fail("provider IMDb identity mismatch"));
            }
            let year = if a["year"].is_null() || a["year"].as_str() == Some("") {
                None
            } else {
                Some(super::series::year(&a["year"])?)
            };
            shows.push(super::ShowCandidate {
                show_id,
                title,
                year,
                imdb_id,
            });
        }
        if count.is_some_and(|n| n < shows.len() as u64) {
            return Err(fail("show search total count inconsistent"));
        }
        let more_pages = advertised.is_some_and(|n| n > page);
        if data.len() > available
            || (more_pages && (page == MAX_PAGES || shows.len() == MAX_RESULTS))
        {
            return Ok((shows, true));
        }
        if !more_pages {
            // A count without enough pages is not evidence of uniqueness.
            let truncated = count.is_some_and(|n| n > shows.len() as u64)
                || (current.is_some() && advertised.is_none());
            return Ok((shows, truncated));
        }
        if data.is_empty() {
            return Err(fail("show search pagination made no progress"));
        }
    }
    unreachable!()
}
#[cfg(test)]
pub(super) fn resolve_season<T: Transport>(
    online: &mut T,
    scope: &Scope,
    show_id: u64,
    show_title: String,
    cache: Option<&super::Cache>,
) -> Result<Manifest> {
    resolve_season_with_choice(online, scope, show_id, show_title, cache, None)
}
pub(super) fn resolve_season_with_choice<T: Transport>(
    online: &mut T,
    scope: &Scope,
    show_id: u64,
    show_title: String,
    cache: Option<&super::Cache>,
    show_choice: Option<super::show_selection::Choice>,
) -> Result<Manifest> {
    let response = online.api(&format!("features?feature_id={show_id}&type=tvshow"), None)?;
    let data = array(&response["data"])?;
    if data.len() != 1 || feature_id(&data[0])? != show_id {
        return Err(fail("show detail identity mismatch"));
    }
    let a = &data[0]["attributes"];
    if text(&a["title"], 200)? != show_title {
        return Err(fail("show detail title mismatch"));
    }
    let seasons = array(&a["seasons"])?;
    let mut episodes: BTreeMap<u32, BTreeMap<u64, String>> = BTreeMap::new();
    let mut identities = BTreeMap::new();
    let mut found = false;
    for season in seasons {
        if counter(&season["season_number"])? != scope.season as u64 {
            continue;
        }
        if found {
            return Err(fail("duplicate season metadata"));
        }
        found = true;
        for episode in array(&season["episodes"])? {
            let number = counter(&episode["episode_number"])? as u32;
            if number == 0 || number > 1000 {
                return Err(fail("invalid episode number"));
            }
            if scope
                .episodes
                .is_some_and(|(a, b)| number < a || number > b)
            {
                continue;
            }
            let identity = id(&episode["feature_id"])?;
            let title = text(&episode["title"], 512)?;
            if let Some(old) = identities.insert(identity, (number, title.clone()))
                && old != (number, title.clone())
            {
                return Err(fail(
                    "conflicting number/title for the same episode identity",
                ));
            }
            // Exact repeated rows collapse; distinct IDs at this number remain competitors.
            episodes.entry(number).or_default().insert(identity, title);
        }
    }
    if episodes.is_empty() || identities.len() > super::MAX_EPISODES as usize {
        return Err(fail(
            "season absent or larger than 1000-reference metadata resource guard; supply --episodes",
        ));
    }
    if let Some((a, b)) = scope.episodes
        && episodes.keys().copied().collect::<Vec<_>>() != (a..=b).collect::<Vec<_>>()
    {
        return Err(fail("requested episode metadata coverage incomplete"));
    }
    let mut selected = Vec::new();
    let mut unavailable = Vec::new();
    for (number, variants) in episodes {
        if variants.len() > 1 {
            println!(
                "S{:02}E{number:02}: checking {} distinct catalog identities.",
                scope.season,
                variants.len()
            );
        }
        let mut chosen = BTreeMap::new();
        if let Some(cache) = cache {
            for (&id, title) in &variants {
                if let Some(existing) = cache.selected(show_id, scope.season, number, id, title)? {
                    chosen.insert(id, existing);
                }
            }
        }
        if chosen.len() < variants.len() {
            // One bounded catalog query per episode number, not per competing identity.
            for (id, candidate) in select(online, show_id, scope.season, number, &variants)? {
                chosen.entry(id).or_insert(candidate);
            }
        }
        // Coverage is by actual episode number, not by every provider metadata ID.
        // Keep every available/frozen identity independently; an unavailable ID
        // is not a subtitle competitor that can be removed to influence matching.
        if chosen.is_empty() {
            return Err(fail(&format!(
                "no eligible independent English S{:02}E{number:02} reference for episode ID {} (none of {} catalog identities available)",
                scope.season,
                variants.keys().next().unwrap(),
                variants.len()
            )));
        }
        for (id, title) in variants {
            if let Some(candidate) = chosen.remove(&id) {
                selected.push(candidate);
            } else {
                unavailable.push(super::UnavailableVariant {
                    episode: number,
                    episode_id: id,
                    title,
                    reason: super::UnavailableReason::NoEligibleEnglishReference,
                });
            }
        }
    }
    let manifest = Manifest {
        policy: POLICY,
        scope: scope.clone(),
        show_id,
        show_title,
        show_choice,
        selected,
        unavailable,
    };
    manifest.validate(scope)?;
    Ok(manifest)
}
fn select<T: Transport>(
    online: &mut T,
    show_id: u64,
    season: u32,
    episode: u32,
    variants: &BTreeMap<u64, String>,
) -> Result<BTreeMap<u64, Selected>> {
    let mut selected = BTreeMap::new();
    for candidate in ranked(online, show_id, season, episode, variants)? {
        selected
            .entry(candidate.selected.episode_id)
            .or_insert(candidate.selected);
    }
    Ok(selected)
}
pub(super) fn alternatives<T: Transport>(
    online: &mut T,
    manifest: &Manifest,
    original: &Selected,
) -> Result<Vec<Selected>> {
    let mut variants = manifest
        .selected
        .iter()
        .filter(|s| s.episode == original.episode)
        .map(|s| (s.episode_id, s.title.clone()))
        .collect::<BTreeMap<_, _>>();
    for v in &manifest.unavailable {
        if v.episode == original.episode {
            variants.insert(v.episode_id, v.title.clone());
        }
    }
    Ok(ranked(
        online,
        original.show_id,
        original.season,
        original.episode,
        &variants,
    )?
    .into_iter()
    .map(|r| r.selected)
    .filter(|s| s.episode_id == original.episode_id)
    .collect())
}
#[derive(Clone)]
struct Ranked {
    selected: Selected,
    trusted: bool,
    sdh: bool,
    downloads: u64,
}
fn ranked<T: Transport>(
    online: &mut T,
    show_id: u64,
    season: u32,
    episode: u32,
    variants: &BTreeMap<u64, String>,
) -> Result<Vec<Ranked>> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut total = None;
    let mut pages = None;
    let mut count = 0;
    for page in 1..=MAX_PAGES {
        let response = online.api(&format!("subtitles?ai_translated=exclude&episode_number={episode}&foreign_parts_only=exclude&languages=en&machine_translated=exclude&page={page}&parent_feature_id={show_id}&season_number={season}&type=episode"), None)?;
        let advertised = counter(&response["total_pages"])?;
        let total_count = counter(&response["total_count"])?;
        let data = array(&response["data"])?;
        if counter(&response["page"])? != page
            || advertised > MAX_PAGES
            || total_count > MAX_RESULTS as u64
            || pages.is_some_and(|n| n != advertised)
            || total.is_some_and(|n| n != total_count)
            || (advertised == 0 && (total_count != 0 || !data.is_empty()))
        {
            return Err(fail(
                "subtitle pagination incomplete/inconsistent or exceeds bounds",
            ));
        }
        pages = Some(advertised);
        total = Some(total_count);
        count += data.len();
        if count > MAX_RESULTS {
            return Err(fail("subtitle result cap exceeded"));
        }
        for value in data {
            let subtitle_id = id(&value["id"])?;
            let a = &value["attributes"];
            if id(&a["subtitle_id"])? != subtitle_id || !seen.insert(subtitle_id) {
                return Err(fail("duplicate/conflicting subtitle identity across pages"));
            }
            let f = &a["feature_details"];
            let episode_id = id(&f["feature_id"])?;
            if a["language"] != "en"
                || id(&f["parent_feature_id"])? != show_id
                || !variants.contains_key(&episode_id)
                || counter(&f["season_number"])? != season as u64
                || counter(&f["episode_number"])? != episode as u64
                || !f["feature_type"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case("episode"))
            {
                return Err(fail(
                    "subtitle response wrong language/parent/episode identity",
                ));
            }
            // Provider episode title must agree, not be derived by splitting a release filename.
            let title = &variants[&episode_id];
            if !titles_agree(&text(&f["title"], 512)?, title) {
                return Err(fail(&format!(
                    "subtitle and episode title labels conflict (S{season:02}E{episode:02}, episode_id={episode_id}, subtitle_id={subtitle_id})"
                )));
            }
            let eligible = ["foreign_parts_only", "ai_translated", "machine_translated"]
                .iter()
                .all(|name| a[*name].as_bool() == Some(false));
            let entries = array(&a["files"])?;
            for file in entries {
                if !files.insert(id(&file["file_id"])?) {
                    return Err(fail("duplicate downloadable file identity"));
                }
            }
            if !eligible || entries.len() != 1 || counter(&entries[0]["cd_number"])? != 1 {
                continue;
            }
            let release = optional_text(&a["release"], 1024)?;
            let comments = match a["comments"].as_str() {
                Some(s)
                    if s.len() <= 4096
                        && !s
                            .chars()
                            .any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t')) =>
                {
                    s.replace(['\r', '\n', '\t'], " ")
                }
                None if a["comments"].is_null() => String::new(),
                _ => return Err(fail("invalid bounded subtitle comments metadata")),
            };
            let filename = optional_text(&entries[0]["file_name"], 1024)?;
            if [&release, &comments, &filename]
                .iter()
                .any(|s| label_conflict(s, season, episode))
            {
                continue;
            }
            let Some(sdh) = a["hearing_impaired"].as_bool() else {
                continue;
            };
            let Some(trusted) = a["from_trusted"].as_bool() else {
                continue;
            };
            let downloads = counter(&a["download_count"])?;
            let uploader = optional_text(&a["uploader"]["name"], 200)?;
            candidates.push(Ranked {
                selected: Selected {
                    show_id,
                    episode_id,
                    season,
                    episode,
                    title: title.clone(),
                    subtitle_id,
                    file_id: id(&entries[0]["file_id"])?,
                    language: "en".into(),
                    release,
                    uploader,
                    source: format!("https://www.opensubtitles.com/en/subtitles/{subtitle_id}"),
                },
                trusted,
                sdh,
                downloads,
            });
        }
        if page >= advertised {
            if count as u64 != total_count {
                return Err(fail(
                    "subtitle pagination total does not match received count",
                ));
            }
            candidates.sort_by_key(|c| {
                (
                    !c.trusted,
                    c.sdh,
                    std::cmp::Reverse(c.downloads),
                    c.selected.subtitle_id,
                    c.selected.file_id,
                )
            });
            return Ok(candidates);
        }
        if data.is_empty() {
            return Err(fail("empty nonfinal subtitle page"));
        }
    }
    Err(fail("subtitle pagination cap exceeded"))
}
// Reject explicit conflicting episode tokens and pack/range indications; never use these to assign IDs.
fn label_conflict(text: &str, season: u32, episode: u32) -> bool {
    let lower = text.to_ascii_lowercase();
    if [
        "complete season",
        "season pack",
        "full season",
        "episodes",
        "saison",
        "season 1-",
        "s01-s",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        return true;
    }
    let bytes = lower.as_bytes();
    let mut labels = 0;
    for i in 0..bytes.len() {
        if bytes[i] != b's' {
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j == i + 1 || j >= bytes.len() || bytes[j] != b'e' {
            continue;
        }
        let season_token = lower[i + 1..j].parse::<u32>().ok();
        let begin = j + 1;
        j = begin;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j == begin {
            continue;
        }
        let episode_token = lower[begin..j].parse::<u32>().ok();
        // A numeric prefix inside an opaque tag (e.g. S01E2l12BC8) is not
        // an E02 assertion. Keep explicit v2 revisions and compact episode
        // continuations recognizable; neither can hide a conflicting label.
        if bytes.get(j) == Some(&b'v') && bytes.get(j + 1).is_some_and(u8::is_ascii_digit) {
            j += 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
        }
        let continuation = matches!(bytes.get(j), Some(b'e' | b's'))
            && bytes.get(j + 1).is_some_and(u8::is_ascii_digit);
        if bytes.get(j).is_some_and(u8::is_ascii_alphanumeric) && !continuation {
            continue;
        }
        labels += 1;
        if season_token != Some(season) || episode_token != Some(episode) {
            return true;
        }
        if continuation
            || (bytes.get(j) == Some(&b'-')
                && bytes
                    .get(j + 1)
                    .is_some_and(|b| b.is_ascii_digit() || *b == b'e' || *b == b's'))
        {
            return true;
        }
    }
    // Common 1x02 labels are validated independently too.
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'x' || i == 0 || !bytes[i - 1].is_ascii_digit() {
            continue;
        }
        let mut a = i;
        while a > 0 && bytes[a - 1].is_ascii_digit() {
            a -= 1;
        }
        let mut z = i + 1;
        while z < bytes.len() && bytes[z].is_ascii_digit() {
            z += 1;
        }
        if z == i + 1 {
            continue;
        }
        labels += 1;
        if lower[a..i].parse::<u32>().ok() != Some(season)
            || lower[i + 1..z].parse::<u32>().ok() != Some(episode)
            || bytes.get(z) == Some(&b'x')
            || (bytes.get(z) == Some(&b'-')
                && bytes
                    .get(z + 1)
                    .is_some_and(|b| b.is_ascii_digit() || *b == b'x'))
        {
            return true;
        }
    }
    labels > 1
}
fn encode(text: &str) -> String {
    let mut result = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char)
            }
            b' ' => result.push('+'),
            _ => {
                use std::fmt::Write;
                write!(result, "%{b:02X}").unwrap();
            }
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn title_character_references_are_single_pass_not_fuzzy_or_markup_parsing() {
        for (a, b) in [
            ("A & B", "A &amp; B"),
            ("A & B", "A &#38; B"),
            ("A & B", "A &#x26; B"),
            ("'\"<>", "&apos;&quot;&lt;&gt;"),
            ("é 😀", "&#233; &#x1F600;"),
            ("A\u{a0}B", "A&nbsp;B"),
        ] {
            assert!(titles_agree(a, b));
            assert!(titles_agree(b, a));
        }
        for (a, b) in [
            ("A & B", "A &amp;amp; B"),
            ("Episode", "episode"),
            ("A B", "A  B"),
            ("A B", "A&nbsp;B"),
            ("A", "<i>A</i>"),
            ("A & B", "A &unknown; B"),
            ("A & B", "A &#xD800; B"),
        ] {
            assert!(!titles_agree(a, b));
        }
        for s in [
            "&unknown;",
            "&amp",
            "&#0;",
            "&#10;",
            "&#xD800;",
            "&#x110000;",
            "&#99999999999999;",
            "&#-1;",
            "&#;",
            "&é;",
            "&;",
        ] {
            assert_eq!(title_characters(s), s);
        }
    }
    #[test]
    fn bounded_ids_and_counters() {
        for v in [
            serde_json::json!(null),
            serde_json::json!(""),
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!("../1"),
        ] {
            assert!(id(&v).is_err());
        }
        assert_eq!(id(&serde_json::json!("123")).unwrap(), 123);
        assert_eq!(counter(&serde_json::json!(8.0)).unwrap(), 8);
        assert!(counter(&serde_json::json!(8.1)).is_err());
    }
    #[test]
    fn explicit_label_conflicts_not_inference() {
        assert!(!label_conflict("Example.S01E02.HDTV", 1, 2));
        for s in [
            "Example.S01E01",
            "s01e02e03",
            "s01e02-03",
            "s01e02-s01e03",
            "complete season",
            "1x03",
        ] {
            assert!(label_conflict(s, 1, 2), "{s}");
        }
    }
    #[test]
    fn x_episode_ranges_are_not_single_episode_labels() {
        assert!(!label_conflict("Original.1x01.HDTV", 1, 1));
        for label in [
            "Original.1x01-02",
            "Original.1x01-1x02",
            "Original.1x01x02",
            "Original.1x01.1x01",
            "Original.S01E01.1x01",
        ] {
            assert!(label_conflict(label, 1, 1), "{label}");
        }
    }
    #[test]
    fn opaque_tag_prefixes_are_not_episode_labels_but_complete_claims_still_are() {
        for release in [
            "[Commie] Psycho-Pass - S01E18 [S01E2l12BC8]",
            "Show.S01E18v2",
            "Show.S01E18 [S01E2hash]",
        ] {
            assert!(!label_conflict(release, 1, 18), "{release}");
        }
        for release in [
            "Show.S01E18 [S01E02]",
            "Show.S01E18 [S01E18]",
            "Show.S01E02v2",
            "Show.S01E18E19",
            "Show.S01E18S01E19",
            "Show.S01E18-19",
            "Show.S01E18-S01E19",
            "Show.S01E18v2E19",
            "Show.S01E18v2-S01E19",
            "Show.S01E18.1x02",
            "Show.S01E18 season pack",
        ] {
            assert!(label_conflict(release, 1, 18), "{release}");
        }
    }
    #[test]
    fn public_query_encoding() {
        assert_eq!(encode("Silicon Valley & ü"), "Silicon+Valley+%26+%C3%BC");
    }
}
