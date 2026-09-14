//! Caller-owned show identity selection; no stdin or fuzzy identity in the library.
use super::{Manifest, Result, Scope, fail};
use serde::{Deserialize, Serialize};

/// Validated provider TV-show metadata, not an inferred identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShowCandidate {
    pub show_id: u64,
    pub title: String,
    pub year: Option<u16>,
    pub imdb_id: Option<u32>,
}
/// At most five candidates, exact names first, then provider result order.
#[derive(Clone, Debug)]
pub struct ShowOffer {
    pub query: String,
    pub candidates: Vec<ShowCandidate>,
    /// Search or display is incomplete; other results may exist beyond these choices.
    pub truncated: bool,
}
/// UI belongs to the caller. `select` returns a one-based displayed option, or
/// None to cancel. It is never called for dry-run, explicit IMDb or a frozen scope.
pub trait ShowInteraction {
    fn offer(&mut self, offer: &ShowOffer) -> Result<()>;
    fn select(&mut self, offer: &ShowOffer) -> Result<Option<usize>>;
}
pub(super) struct Options<'a> {
    pub dry_run: bool,
    pub interaction: &'a mut dyn ShowInteraction,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Choice {
    policy: String,
    query: String,
    show_id: u64,
    title: String,
}
impl Choice {
    pub fn new(scope: &Scope, candidate: &ShowCandidate) -> Option<Self> {
        (!candidate.title.eq_ignore_ascii_case(&scope.show)).then(|| Self {
            policy: "explicit-show-choice-v1".into(),
            query: scope.show.clone(),
            show_id: candidate.show_id,
            title: candidate.title.clone(),
        })
    }
    pub fn matches(&self, manifest: &Manifest) -> Result<()> {
        if self.policy != "explicit-show-choice-v1"
            || manifest.scope.imdb.is_some()
            || self.query != manifest.scope.show
            || self.show_id != manifest.show_id
            || self.title != manifest.show_title
            || self.title.eq_ignore_ascii_case(&self.query)
        {
            return Err(fail("explicit show choice scope/identity mismatch"));
        }
        Ok(())
    }
}
pub(super) struct Resolution {
    pub id: u64,
    pub title: String,
    pub choice: Option<Choice>,
}
impl Resolution {
    pub fn automatic(candidate: ShowCandidate) -> Self {
        Self {
            id: candidate.show_id,
            title: candidate.title,
            choice: None,
        }
    }
}
pub(super) fn choose(
    scope: &Scope,
    candidates: Vec<ShowCandidate>,
    truncated: bool,
    options: Option<&mut Options<'_>>,
) -> Result<Resolution> {
    let mut exact = candidates
        .iter()
        .filter(|c| c.title.eq_ignore_ascii_case(&scope.show));
    if !truncated
        && let Some(candidate) = exact.next()
        && exact.next().is_none()
    {
        return Ok(Resolution::automatic(candidate.clone()));
    }
    let Some(options) = options else {
        return Err(fail(
            "show resolution absent, ambiguous or truncated; use --imdb ttID",
        ));
    };
    let mut candidates = candidates;
    candidates.sort_by_key(|c| !c.title.eq_ignore_ascii_case(&scope.show));
    let truncated = truncated || candidates.len() > 5;
    candidates.truncate(5);
    let offer = ShowOffer {
        query: scope.show.clone(),
        candidates,
        truncated,
    };
    options.interaction.offer(&offer)?;
    if options.dry_run {
        return Err(fail(
            "show unresolved; dry-run lists choices only, use --imdb ttID",
        ));
    }
    if offer.candidates.is_empty() {
        return Err(fail("no validated TV-show candidates; use --imdb ttID"));
    }
    let index = options
        .interaction
        .select(&offer)?
        .and_then(|n| n.checked_sub(1));
    let candidate = index
        .and_then(|i| offer.candidates.get(i))
        .ok_or_else(|| fail("show selection cancelled or invalid; use --imdb ttID"))?;
    Ok(Resolution {
        id: candidate.show_id,
        title: candidate.title.clone(),
        choice: Choice::new(scope, candidate),
    })
}
