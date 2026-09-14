//! Immutable, canonical empty-reference fallback approvals. Manifests remain frozen.
use super::*;
use crate::opensubtitles::records::parser::{self, RecordError};
pub(in crate::opensubtitles) const MAX_FALLBACKS: usize = 3;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::opensubtitles) struct EmptyProof {
    pub bytes: usize,
    pub sha256: String,
    pub records: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::opensubtitles) struct Approval {
    policy: String,
    pub origin: Selected,
    pub from: Selected,
    pub candidate: Selected,
    pub proof: EmptyProof,
    pub step: usize,
    approved_unix_seconds: u64,
}
pub(in crate::opensubtitles) struct State {
    pub current: Selected,
    pub pending: Option<Approval>,
    pub next_step: usize,
    pub excluded: std::collections::BTreeSet<u64>,
}
fn same_episode(a: &Selected, b: &Selected) -> bool {
    (
        a.show_id,
        a.season,
        a.episode,
        a.episode_id,
        &a.title,
        &a.language,
    ) == (
        b.show_id,
        b.season,
        b.episode,
        b.episode_id,
        &b.title,
        &b.language,
    )
}
impl Approval {
    fn validate(&self) -> Result<()> {
        for s in [&self.origin, &self.from, &self.candidate] {
            s.validate(self.origin.show_id, self.origin.season)?;
        }
        if self.policy != "consented-empty-reference-fallback-v1"
            || !(1..=MAX_FALLBACKS).contains(&self.step)
            || self.approved_unix_seconds == 0
            || !same_episode(&self.origin, &self.from)
            || !same_episode(&self.origin, &self.candidate)
            || self.candidate.file_id == self.from.file_id
            || self.candidate.subtitle_id == self.from.subtitle_id
            || self.proof.bytes == 0
            || self.proof.bytes > crate::srt::MAX_SRT_BYTES
            || !(1..=crate::srt::MAX_CUES).contains(&self.proof.records)
            || self.proof.sha256.len() != 64
            || !self
                .proof
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(fail("invalid fallback approval"));
        }
        Ok(())
    }
}
pub(super) fn owned_name(name: &str) -> bool {
    let Some(stem) = name
        .strip_suffix(".json")
        .or_else(|| name.strip_suffix(".partial"))
    else {
        return false;
    };
    let Some(rest) = stem.strip_prefix("fallback-") else {
        return false;
    };
    let Some((id, step)) = rest.split_once('-') else {
        return false;
    };
    numeric(&format!("file-{id}"), "file-") && matches!(step, "1" | "2" | "3")
}
impl Cache {
    fn fallback_path(&self, origin: &Selected, step: usize) -> Result<PathBuf> {
        Ok(self
            .entry(origin)?
            .parent()
            .unwrap()
            .join(format!("fallback-{}-{step}.json", origin.file_id)))
    }
    pub(super) fn inventory_approval(&self, path: &Path, inv: &mut Inventory) -> Result<()> {
        let a: Approval = serde_json::from_slice(&read(path, MANIFEST_CAP)?)
            .map_err(|_| fail("invalid fallback approval metadata"))?;
        a.validate()?;
        if self.fallback_path(&a.origin, a.step)? != path {
            return Err(fail("fallback approval path mismatch"));
        }
        // Preserve both successful replacements and failed histories. They are
        // bounded by the same cache cap, not silently evicted or redownloaded.
        for s in [&a.origin, &a.from, &a.candidate] {
            inv.protected.insert(self.entry(s)?);
        }
        Ok(())
    }
    pub(in crate::opensubtitles) fn empty_proof(&self, s: &Selected) -> Result<Option<EmptyProof>> {
        let _transaction = self.locks.transaction()?;
        self.lock_season(s.show_id, s.season)?;
        let entry = self.entry(s)?;
        if exists(&entry)? {
            self.load(s)?;
            return Ok(None);
        }
        let partial = entry.with_extension("partial");
        if !exists(&partial)? {
            return Ok(None);
        }
        let marker = entry.with_extension("attempt");
        if exists(&marker)? && read(&marker, MANIFEST_CAP)? != ATTEMPT {
            return Err(fail("empty reference attempt marker mismatch"));
        }
        if exists(&partial.join("provenance.json"))? {
            return Err(fail("empty fallback refuses staged provenance"));
        }
        let receipt: DownloadReceipt =
            serde_json::from_slice(&read(&partial.join("download.json"), MANIFEST_CAP)?)
                .map_err(|_| fail("empty reference receipt invalid"))?;
        let raw = read(&partial.join("content.srt"), crate::srt::MAX_SRT_BYTES)?;
        if receipt.version != 1
            || receipt.selected != *s
            || receipt.bytes != raw.len()
            || receipt.sha256 != digest(&raw)
        {
            return Err(fail("empty reference receipt/body mismatch"));
        }
        let decoded = crate::srt::layout::decode(&raw)
            .map_err(|e| fail(&format!("empty reference decoding: {e}")))?;
        let records = parser::parse(&decoded.text)?;
        if records.is_empty()
            || !records
                .iter()
                .all(|r| matches!(r, Err(RecordError::MissingText(_))))
        {
            return Ok(None);
        }
        Ok(Some(EmptyProof {
            bytes: raw.len(),
            sha256: digest(&raw),
            records: records.len(),
        }))
    }
    pub(in crate::opensubtitles) fn fallback_state(&self, origin: &Selected) -> Result<State> {
        let _transaction = self.locks.transaction()?;
        self.lock_season(origin.show_id, origin.season)?;
        let mut approvals = Vec::new();
        let mut gap = false;
        for step in 1..=MAX_FALLBACKS {
            let path = self.fallback_path(origin, step)?;
            if exists(&path.with_extension("partial"))? {
                return Err(fail(
                    "incomplete fallback approval retained; no download permitted",
                ));
            }
            if !exists(&path)? {
                gap = true;
                continue;
            }
            if gap {
                return Err(fail("nonconsecutive fallback approval history"));
            }
            let a: Approval = serde_json::from_slice(&read(&path, MANIFEST_CAP)?)
                .map_err(|_| fail("invalid fallback approval metadata"))?;
            a.validate()?;
            if a.origin != *origin || a.step != step {
                return Err(fail("fallback origin/step mismatch"));
            }
            approvals.push(a);
        }
        let count = approvals.len();
        let mut current = origin.clone();
        let mut excluded = std::collections::BTreeSet::from([origin.file_id]);
        for a in approvals {
            if a.from != current
                || self.empty_proof(&current)?.as_ref() != Some(&a.proof)
                || !excluded.insert(a.candidate.file_id)
            {
                return Err(fail("fallback history/empty proof mismatch"));
            }
            match self.recover(&a.candidate) {
                Ok(true) => {
                    if a.step != count {
                        return Err(fail("fallback cannot replace usable reference content"));
                    }
                    current = a.candidate;
                }
                Ok(false) => {
                    if a.step != count {
                        return Err(fail("fallback pending history conflict"));
                    }
                    return Ok(State {
                        current,
                        pending: Some(a),
                        next_step: count + 1,
                        excluded,
                    });
                }
                Err(e) => {
                    if self.empty_proof(&a.candidate)?.is_none() {
                        return Err(e);
                    }
                    current = a.candidate;
                }
            }
        }
        Ok(State {
            current,
            pending: None,
            next_step: count + 1,
            excluded,
        })
    }
    pub(in crate::opensubtitles) fn effective_manifest(&self, base: &Manifest) -> Result<Manifest> {
        let mut m = base.clone();
        for s in &mut m.selected {
            *s = self.fallback_state(s)?.current;
        }
        m.validate(&m.scope)?;
        Ok(m)
    }
    pub(in crate::opensubtitles) fn approve_fallback(
        &self,
        origin: &Selected,
        from: &Selected,
        candidate: &Selected,
        proof: &EmptyProof,
    ) -> Result<Approval> {
        let _transaction = self.locks.transaction()?;
        let state = self.fallback_state(origin)?;
        if state.current != *from || self.empty_proof(from)?.as_ref() != Some(proof) {
            return Err(fail("fallback source changed after confirmation"));
        }
        if let Some(a) = state.pending {
            if a.candidate != *candidate {
                return Err(fail("pending fallback candidate is frozen"));
            }
            return Ok(a);
        }
        if state.next_step > MAX_FALLBACKS || state.excluded.contains(&candidate.file_id) {
            return Err(fail("fallback history limit/repeated candidate"));
        }
        let a = Approval {
            policy: "consented-empty-reference-fallback-v1".into(),
            origin: origin.clone(),
            from: from.clone(),
            candidate: candidate.clone(),
            proof: proof.clone(),
            step: state.next_step,
            approved_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| fail("invalid clock"))?
                .as_secs(),
        };
        a.validate()?;
        let bytes = serde_json::to_vec_pretty(&a)
            .map_err(|_| fail("fallback approval serialization failed"))?;
        if bytes.len() > MANIFEST_CAP {
            return Err(fail("fallback approval byte cap"));
        }
        // Approval is extra protected state, not credited against the payload's
        // worst-case reservation. It must fit in addition to that reservation.
        self.space(bytes.len() as u64)?;
        let path = self.fallback_path(origin, a.step)?;
        ensure_dir(path.parent().unwrap())?;
        atomic_file(&path, &bytes)?;
        Ok(a)
    }
}
