//! Explicit consent for structural-empty references, never score-driven shopping.
use super::*;
use cache::fallback::MAX_FALLBACKS;

/// A bounded ranked list. The proposed item is the only possible download this call.
#[derive(Clone, Debug)]
pub struct FallbackOffer {
    pub failed: Selected,
    pub empty_records: usize,
    pub alternatives: Vec<Selected>,
    pub proposed: Selected,
    pub retry: bool,
    pub attempt: usize,
}
/// UI belongs to the caller. `confirm` is never called in dry-run mode. A true
/// response authorizes one possibly quota-consuming POST, not further fallbacks.
pub trait FallbackInteraction {
    fn offer(&mut self, offer: &FallbackOffer) -> Result<()>;
    fn confirm(&mut self, offer: &FallbackOffer) -> Result<bool>;
}
pub(super) struct Options<'a> {
    pub dry_run: bool,
    pub interaction: &'a mut dyn FallbackInteraction,
}
pub(super) fn attempt<T: transport::Transport>(
    cache: &Cache,
    base: &Manifest,
    online: &mut T,
    options: &mut Options<'_>,
) -> Result<Manifest> {
    let mut current = cache.effective_manifest(base)?;
    let mut empty = Vec::new();
    // Do not spend replacement quota while a different malformed/missing reference
    // would block coverage. All remaining failures must be proven caption-empty.
    for (i, s) in current.selected.iter().enumerate() {
        match cache.recover(s) {
            Ok(true) => {}
            Ok(false) => {
                return Err(fail(
                    "reference coverage incomplete; missing download prevents empty-reference fallback",
                ));
            }
            Err(e) => match cache.empty_proof(s)? {
                Some(proof) => empty.push((i, proof)),
                None => return Err(e),
            },
        }
    }
    let Some((index, proof)) = empty.into_iter().next() else {
        return Ok(current);
    };
    let origin = &base.selected[index];
    let state = cache.fallback_state(origin)?;
    let (alternatives, proposed, retry, step) = if let Some(a) = state.pending {
        (vec![a.candidate.clone()], a.candidate, true, a.step)
    } else {
        if state.next_step > MAX_FALLBACKS {
            return Err(fail(
                "empty-reference fallback limit reached (3 approved alternatives); all history retained",
            ));
        }
        let alternatives = catalog::alternatives(online, base, origin)?
            .into_iter()
            .filter(|s| {
                !state.excluded.contains(&s.file_id)
                    && !current
                        .selected
                        .iter()
                        .any(|other| other.episode_id != s.episode_id && other.file_id == s.file_id)
            })
            .take(3)
            .collect::<Vec<_>>();
        let proposed = alternatives.first().cloned().ok_or_else(|| {
            fail("no remaining eligible English fallback; empty reference retained")
        })?;
        (alternatives, proposed, false, state.next_step)
    };
    let offer = FallbackOffer {
        failed: state.current.clone(),
        empty_records: proof.records,
        alternatives,
        proposed: proposed.clone(),
        retry,
        attempt: step,
    };
    options.interaction.offer(&offer)?;
    if options.dry_run {
        return Err(fail(
            "reference coverage incomplete; dry-run lists fallback only, no confirmation or replacement download",
        ));
    }
    if online.quota()? == Some(0) {
        return Err(fail("server quota exhausted; no fallback download POST"));
    }
    if !options.interaction.confirm(&offer)? {
        return Err(fail(
            "reference coverage incomplete; fallback declined, no replacement download",
        ));
    }
    current.selected[index] = proposed.clone();
    current.validate(&current.scope)?;
    cache.prepare(&current)?;
    // Revalidate the empty proof/history after user think time. Approval is durable
    // before the charge marker/POST; interrupted attempts need fresh confirmation.
    cache.approve_fallback(origin, &state.current, &proposed, &proof)?;
    if cache.recover(&proposed)? {
        // A prior complete response may already exist locally. Approval changes
        // selection, but never authorizes redownloading retained content.
        return finish(cache, base);
    }
    cache.reserve(&proposed)?;
    let response = online.api(
        "download",
        Some(serde_json::json!({"file_id":proposed.file_id,"sub_format":"srt"})),
    )?;
    let remaining = catalog::counter(&response["remaining"])?;
    println!("Fallback download quota: {remaining} remaining.");
    let link = response["link"].as_str().ok_or_else(|| {
        fail("fallback download link missing; charge uncertain; approval/attempt retained")
    })?;
    let raw = online.content(link)?;
    cache.publish(&proposed, &raw).map_err(|e| {
        fail(&format!(
            "fallback file_id={}: {e}; no automatic next attempt",
            proposed.file_id
        ))
    })?;
    finish(cache, base)
}
fn finish(cache: &Cache, base: &Manifest) -> Result<Manifest> {
    let effective = cache.effective_manifest(base)?;
    let (pending, failures) = recover_missing(cache, &effective)?;
    if !pending.is_empty() || !failures.is_empty() {
        return Err(fail(&format!(
            "one fallback completed; remaining reference coverage incomplete; {}",
            failures.join("; ")
        )));
    }
    println!("Fallback reference validated; original empty bytes and approval history retained.");
    Ok(effective)
}
