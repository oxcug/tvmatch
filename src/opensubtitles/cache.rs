#[cfg(test)]
mod concurrency;
pub(super) mod fallback;
mod locking;
use super::{Manifest, Reference, ReferenceId, Result, Scope, Selected, fail, transcript};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
const MANIFEST_CAP: usize = 256 * 1024;
const CACHE_CAP: u64 = 250 * 1024 * 1024;
const ATTEMPT: &[u8] = b"A download POST may have been charged. No automatic replay. Owner review required if content missing.
";
#[derive(Default)]
struct Inventory {
    total: u64,
    count: usize,
    entries: Vec<(u64, PathBuf, u64)>,
    manifests: Vec<PathBuf>,
    protected: std::collections::BTreeSet<PathBuf>,
}
fn numeric(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|s| {
        s.parse::<u64>()
            .ok()
            .is_some_and(|n| n > 0 && n <= i32::MAX as u64 && n.to_string() == s)
    })
}
fn request_name(name: &str) -> bool {
    name.strip_prefix("request-")
        .and_then(|n| {
            n.strip_suffix(".json")
                .or_else(|| n.strip_suffix(".partial"))
        })
        .is_some_and(|n| {
            n.len() == 64
                && n.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}
/// Canonical show/season ownership spans reference work; short shared transactions
/// protect cache IO and capacity, never network calls. Not a same-user sandbox.
pub struct Cache {
    root: PathBuf,
    locks: locking::Locks,
    cap: u64,
    pins: std::cell::RefCell<std::collections::BTreeSet<PathBuf>>,
    attempted: std::cell::RefCell<std::collections::BTreeSet<PathBuf>>,
}
impl Cache {
    pub fn default_private() -> Result<Self> {
        Self::open(
            &crate::paths::reference_cache().map_err(|_| fail("invalid user cache directory"))?,
        )
    }
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_with_cap(root, CACHE_CAP)
    }
    fn open_with_cap(root: &Path, cap: u64) -> Result<Self> {
        if !root.is_absolute() {
            return Err(fail("cache requires absolute private directory"));
        }
        ensure_dir(root)?;
        let locks = locking::Locks::open(root)?;
        let cache = Self {
            root: root.to_owned(),
            locks,
            cap,
            pins: Default::default(),
            attempted: Default::default(),
        };
        cache.space(0)?;
        Ok(cache)
    }
    pub(super) fn lock_season(&self, show: u64, season: u32) -> Result<()> {
        self.locks.claim((show, season))
    }
    pub(super) fn selected(
        &self,
        show: u64,
        season: u32,
        episode: u32,
        id: u64,
        title: &str,
    ) -> Result<Option<Selected>> {
        let _transaction = self.locks.transaction()?;
        self.lock_season(show, season)?;
        let mut found = None;
        for path in self.inventory()?.manifests {
            let m: Manifest = serde_json::from_slice(&read(&path, MANIFEST_CAP)?)
                .map_err(|_| fail("invalid cached manifest"))?;
            m.validate(&m.scope)?;
            for s in m.selected {
                if s.show_id == show && s.episode_id == id {
                    if s.season != season
                        || s.episode != episode
                        || s.title != title
                        || found.as_ref().is_some_and(|old| old != &s)
                    {
                        return Err(fail("frozen episode identity conflict"));
                    }
                    found = Some(s);
                }
            }
        }
        Ok(found)
    }
    #[cfg(test)]
    pub(super) fn alias(
        &self,
        scope: &Scope,
        show_id: u64,
        title: &str,
    ) -> Result<Option<Manifest>> {
        self.alias_with_choice(scope, show_id, title, None)
    }
    pub(super) fn alias_with_choice(
        &self,
        scope: &Scope,
        show_id: u64,
        title: &str,
        choice: Option<super::show_selection::Choice>,
    ) -> Result<Option<Manifest>> {
        let _transaction = self.locks.transaction()?;
        self.lock_season(show_id, scope.season)?;
        let inventory = self.inventory()?;
        let mut matches = Vec::new();
        for path in inventory.manifests {
            let mut m: Manifest = serde_json::from_slice(&read(&path, MANIFEST_CAP)?)
                .map_err(|_| fail("invalid cached manifest"))?;
            m.validate(&m.scope)?;
            if m.show_id != show_id || m.show_title != title || m.scope.season != scope.season {
                continue;
            }
            // A range manifest is not evidence of the whole provider season.
            if scope.episodes.is_none() && m.scope.episodes.is_some() {
                continue;
            }
            if let Some((a, b)) = scope.episodes {
                m.selected.retain(|s| s.episode >= a && s.episode <= b);
                m.unavailable.retain(|s| s.episode >= a && s.episode <= b);
            }
            if scope.imdb.is_some() || title.eq_ignore_ascii_case(&scope.show) {
                m.show_choice = None;
            } else if let Some(choice) = &choice {
                m.show_choice = Some(choice.clone());
            }
            m.scope = scope.clone();
            if m.validate(scope).is_ok() {
                matches.push(m);
            }
        }
        if matches
            .windows(2)
            .any(|w| w[0].selected != w[1].selected || w[0].unavailable != w[1].unavailable)
        {
            return Err(fail("cached aliases conflict"));
        }
        Ok(matches.into_iter().next())
    }
    pub(super) fn pin(&self, manifest: &Manifest) -> Result<()> {
        let _transaction = self.locks.transaction()?;
        manifest.validate(&manifest.scope)?;
        self.lock_season(manifest.show_id, manifest.scope.season)?;
        let mut pins = self.pins.borrow_mut();
        pins.clear();
        pins.insert(self.request_path(&manifest.scope)?);
        for s in &manifest.selected {
            pins.insert(self.entry(s)?);
        }
        Ok(())
    }
    pub(super) fn prepare(&self, manifest: &Manifest) -> Result<()> {
        let _transaction = self.locks.transaction()?;
        self.pin(manifest)?;
        // Publish outstanding worst-case capacity BEFORE any POST. Other seasons
        // must honor it even while this process waits on the provider/CDN.
        let key = (manifest.show_id, manifest.scope.season);
        let missing = self.missing(manifest)?.len() as u64;
        let bytes = missing
            * (crate::srt::MAX_SRT_BYTES as u64 + 2 * MANIFEST_CAP as u64 + ATTEMPT.len() as u64);
        let old = self.locks.budget(key)?;
        if bytes > old {
            self.space(bytes - old)?;
        }
        self.locks.reserve(key, bytes)
    }
    fn inventory(&self) -> Result<Inventory> {
        let _transaction = self.locks.transaction()?;
        let mut inv = Inventory::default();
        self.walk(&self.root, 0, &mut inv)?;
        inv.entries.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        inv.manifests.sort();
        Ok(inv)
    }
    fn walk(&self, dir: &Path, depth: usize, inv: &mut Inventory) -> Result<()> {
        if depth > 4 {
            return Err(fail("cache depth exceeded"));
        }
        check_ancestors(dir)?;
        for child in fs::read_dir(dir).map_err(|_| fail("cache inventory failed"))? {
            inv.count += 1;
            if inv.count > 100_000 {
                return Err(fail("cache inventory count exceeded"));
            }
            let child = child.map_err(|_| fail("cache inventory failed"))?;
            let path = child.path();
            let name = child.file_name();
            let Some(name) = name.to_str() else {
                if depth == 4 {
                    return Err(fail("unowned file inside cache content entry"));
                }
                continue;
            };
            if depth == 0 && name == ".lock" {
                continue;
            }
            let directory = match depth {
                0 => numeric(name, "show-"),
                1 => numeric(name, "season-"),
                2 => numeric(name, "episode-"),
                3 => {
                    numeric(name, "file-")
                        || name
                            .strip_suffix(".partial")
                            .is_some_and(|n| numeric(n, "file-"))
                }
                _ => false,
            };
            let manifest = depth == 0 && request_name(name);
            let marker = depth == 3
                && name
                    .strip_suffix(".attempt")
                    .is_some_and(|n| numeric(n, "file-"));
            let display = depth == 0
                && name
                    .strip_suffix(".json")
                    .is_some_and(|s| numeric(s, "series-"));
            let fallback = depth == 3 && fallback::owned_name(name);
            let owned_file = manifest
                || fallback
                || display
                || marker
                || (depth == 4
                    && (matches!(name, "content.srt" | "provenance.json")
                        || (name == "download.json"
                            && dir.extension() == Some(std::ffi::OsStr::new("partial")))));
            if !directory && !owned_file {
                // Never traverse or delete unowned paths. Unknown files inside content prevent eviction.
                if depth == 4 {
                    return Err(fail("unowned file inside cache content entry"));
                }
                continue;
            }
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| fail("cache inventory metadata failed"))?;
            refuse_link(&metadata)?;
            if directory {
                if !metadata.is_dir() {
                    return Err(fail("cache directory collision"));
                }
                self.walk(&path, depth + 1, inv)?;
                if depth == 3 && numeric(name, "file-") {
                    let p: Provenance =
                        serde_json::from_slice(&read(&path.join("provenance.json"), MANIFEST_CAP)?)
                            .map_err(|_| fail("invalid cache provenance"))?;
                    if self.entry(&p.selected)? != path {
                        return Err(fail("cache entry path identity mismatch"));
                    }
                    self.load(&p.selected)?;
                    let marker = path.with_extension("attempt");
                    let marker_bytes = if exists(&marker)? {
                        let bytes = read(&marker, MANIFEST_CAP)?;
                        if bytes != ATTEMPT {
                            return Err(fail("successful entry marker malformed"));
                        }
                        bytes.len() as u64
                    } else {
                        0
                    };
                    inv.entries.push((
                        p.fetched_unix_seconds,
                        path,
                        p.bytes as u64
                            + fs::metadata(child.path().join("provenance.json"))
                                .map_err(|_| fail("cache metadata missing"))?
                                .len()
                            + marker_bytes,
                    ));
                }
            } else {
                if !metadata.is_file() {
                    return Err(fail("cache file is not regular"));
                }
                inv.total = inv
                    .total
                    .checked_add(metadata.len())
                    .ok_or_else(|| fail("cache size overflow"))?;
                if fallback && name.ends_with(".json") {
                    self.inventory_approval(&path, inv)?;
                }
                if manifest && name.ends_with(".json") {
                    let m: Manifest = serde_json::from_slice(&read(&path, MANIFEST_CAP)?)
                        .map_err(|_| fail("invalid cached manifest"))?;
                    m.validate(&m.scope)?;
                    if self.request_path(&m.scope)? != path {
                        return Err(fail("manifest path identity mismatch"));
                    }
                    inv.manifests.push(path);
                }
            }
        }
        Ok(())
    }
    fn space(&self, additional: u64) -> Result<()> {
        self.space_for(additional, None)
    }
    fn space_for(&self, additional: u64, consuming: Option<locking::Season>) -> Result<()> {
        let _transaction = self.locks.transaction()?;
        let active = self.locks.active()?;
        let outstanding = active.values().try_fold(0u64, |sum, n| {
            sum.checked_add(*n)
                .ok_or_else(|| fail("cache reservation overflow"))
        })?;
        let credit = consuming
            .and_then(|key| active.get(&key))
            .copied()
            .unwrap_or(0)
            .min(additional);
        let inv = self.inventory()?;
        let target = inv
            .total
            .checked_add(outstanding)
            .and_then(|n| n.checked_add(additional - credit))
            .ok_or_else(|| fail("cache size overflow"))?;
        if target <= self.cap {
            return Ok(());
        }
        let pins = self.pins.borrow();
        let mut reclaimed = 0;
        let mut victims = Vec::new();
        for (_, path, size) in inv.entries {
            if pins.contains(&path)
                || inv.protected.contains(&path)
                || active.keys().any(|key| {
                    !self.locks.owns(*key)
                        && path.starts_with(
                            self.root
                                .join(format!("show-{}", key.0))
                                .join(format!("season-{}", key.1)),
                        )
                })
            {
                continue;
            }
            reclaimed += size;
            victims.push((path, true));
            if target - reclaimed <= self.cap {
                break;
            }
        }
        // Frozen metadata is disposable only when capacity requires it, never active scopes.
        // Insertion mtime, not reads, orders manifests; content always uses fetch time.
        if target - reclaimed > self.cap {
            let mut manifests = Vec::new();
            for path in inv.manifests {
                if pins.contains(&path) {
                    continue;
                }
                let manifest: Manifest = serde_json::from_slice(&read(&path, MANIFEST_CAP)?)
                    .map_err(|_| fail("invalid cached manifest"))?;
                let key = (manifest.show_id, manifest.scope.season);
                if active.contains_key(&key) && !self.locks.owns(key) {
                    continue;
                }
                // Do not discard a frozen selection while any of its content survives.
                if manifest
                    .selected
                    .iter()
                    .any(|s| self.entry(s).is_ok_and(|p| p.exists()))
                {
                    continue;
                }
                let mut protected = false;
                for s in &manifest.selected {
                    let entry = self.entry(s)?;
                    protected |= exists(&entry.with_extension("attempt"))?
                        || exists(&entry.with_extension("partial"))?;
                }
                if protected {
                    continue;
                }
                let m = fs::metadata(&path).map_err(|_| fail("manifest metadata missing"))?;
                manifests.push((
                    m.modified()
                        .map_err(|_| fail("manifest timestamp missing"))?,
                    path,
                    m.len(),
                ));
            }
            manifests.sort();
            for (_, path, size) in manifests {
                reclaimed += size;
                victims.push((path, false));
                if target - reclaimed <= self.cap {
                    break;
                }
            }
        }
        if target - reclaimed > self.cap {
            return Err(fail(
                "reference cache cap cannot fit protected attempts/staging or active working set; no download POST permitted",
            ));
        }
        for (path, entry) in victims {
            if entry {
                fs::remove_file(path.join("content.srt"))
                    .map_err(|_| fail("cache eviction failed"))?;
                fs::remove_file(path.join("provenance.json"))
                    .map_err(|_| fail("cache eviction failed"))?;
                fs::remove_dir(&path).map_err(|_| fail("cache eviction failed"))?;
                // Only a verified successful entry authorizes clearing its charge marker.
                let marker = path.with_extension("attempt");
                if exists(&marker)? {
                    fs::remove_file(marker)
                        .map_err(|_| fail("successful marker eviction failed"))?;
                }
                let mut parent = path.parent();
                while let Some(dir) = parent {
                    if dir == self.root || fs::remove_dir(dir).is_err() {
                        break;
                    }
                    parent = dir.parent();
                }
            } else {
                fs::remove_file(path).map_err(|_| fail("manifest eviction failed"))?;
            }
        }
        Ok(())
    }
    fn request_path(&self, scope: &Scope) -> Result<PathBuf> {
        scope.validate()?;
        let bytes = serde_json::to_vec(scope).map_err(|_| fail("scope serialization failed"))?;
        Ok(self.root.join(format!("request-{}.json", digest(&bytes))))
    }
    /// Frozen scope metadata for episode coverage; no acquisition or inferred numbering.
    pub fn selected_references(&self, scope: &Scope) -> Result<Vec<Selected>> {
        let _transaction = self.locks.transaction()?;
        scope.validate()?;
        self.manifest(scope)?
            .map(|m| m.selected)
            .ok_or_else(|| fail("cached selection missing"))
    }
    pub(super) fn display(&self, m: &Manifest) -> Result<Option<super::series::Display>> {
        let _transaction = self.locks.transaction()?;
        m.validate(&m.scope)?;
        let path = self.root.join(format!("series-{}.json", m.show_id));
        if !exists(&path)? {
            return Ok(None);
        }
        let d: super::series::Display = serde_json::from_slice(&read(&path, 4096)?)
            .map_err(|_| fail("invalid cached series display metadata"))?;
        d.matches(m)?;
        Ok(Some(d))
    }
    pub(super) fn store_display(&self, m: &Manifest, d: &super::series::Display) -> Result<()> {
        let _transaction = self.locks.transaction()?;
        d.matches(m)?;
        if let Some(old) = self.display(m)? {
            return if old == *d {
                Ok(())
            } else {
                Err(fail(
                    "concurrent series display metadata conflict; cached value retained",
                ))
            };
        }
        let data = serde_json::to_vec_pretty(d)
            .map_err(|_| fail("series metadata serialization failed"))?;
        if data.len() > 4096 {
            return Err(fail("series metadata byte cap exceeded"));
        }
        self.space(data.len() as u64)?;
        new_file(&self.root.join(format!("series-{}.json", m.show_id)), &data)
    }
    pub fn series_prefix(&self, scope: &Scope, name_override: Option<&str>) -> Result<String> {
        let m = self
            .manifest(scope)?
            .ok_or_else(|| fail("series scope not cached"))?;
        self.display(&m)?
            .ok_or_else(|| fail("series original title/year not cached"))?
            .prefix(name_override)
    }
    /// Legacy display-name accessor; rename previews use `series_prefix`.
    /// Series display name from verified cached metadata; never performs HTTP.
    /// Preserve supplied spelling, or reuse a verified name alias for an IMDb scope.
    pub fn series_name(&self, scope: &Scope) -> Result<String> {
        let _transaction = self.locks.transaction()?;
        let manifest = self
            .manifest(scope)?
            .ok_or_else(|| fail("series metadata is not cached"))?;
        if !scope.show.is_empty() {
            return Ok(scope.show.clone());
        }
        for path in self.inventory()?.manifests {
            let alias: Manifest = serde_json::from_slice(&read(&path, MANIFEST_CAP)?)
                .map_err(|_| fail("invalid cached manifest"))?;
            alias.validate(&alias.scope)?;
            if alias.show_id == manifest.show_id
                && alias.show_title == manifest.show_title
                && !alias.scope.show.is_empty()
            {
                return Ok(alias.scope.show);
            }
        }
        Ok(manifest.show_title)
    }
    pub(super) fn manifest(&self, scope: &Scope) -> Result<Option<Manifest>> {
        let _transaction = self.locks.transaction()?;
        let path = self.request_path(scope)?;
        if !exists(&path)? {
            return Ok(None);
        }
        let manifest: Manifest = serde_json::from_slice(&read(&path, MANIFEST_CAP)?)
            .map_err(|_| fail("invalid cached manifest"))?;
        manifest.validate(scope)?;
        self.lock_season(manifest.show_id, scope.season)?;
        Ok(Some(manifest))
    }
    pub(super) fn freeze(&self, manifest: &Manifest) -> Result<()> {
        let _transaction = self.locks.transaction()?;
        manifest.validate(&manifest.scope)?;
        let key = (manifest.show_id, manifest.scope.season);
        self.lock_season(key.0, key.1)?;
        let path = self.request_path(&manifest.scope)?;
        let data = serde_json::to_vec_pretty(manifest)
            .map_err(|_| fail("manifest serialization failed"))?;
        if data.len() > MANIFEST_CAP {
            return Err(fail("manifest cap exceeded"));
        }
        self.pins.borrow_mut().insert(path.clone());
        for selected in &manifest.selected {
            self.pins.borrow_mut().insert(self.entry(selected)?);
        }
        self.space_for(data.len() as u64, Some(key))?;
        atomic_file(&path, &data)?;
        self.locks.consume(key, data.len() as u64)
    }
    fn entry(&self, s: &Selected) -> Result<PathBuf> {
        s.validate(s.show_id, s.season)?;
        Ok(self
            .root
            .join(format!("show-{}", s.show_id))
            .join(format!("season-{}", s.season))
            .join(format!("episode-{}", s.episode))
            .join(format!("file-{}", s.file_id)))
    }
    pub(super) fn contains(&self, s: &Selected) -> Result<bool> {
        let _transaction = self.locks.transaction()?;
        self.lock_season(s.show_id, s.season)?;
        let entry = self.entry(s)?;
        check_ancestors(&entry)?;
        if !exists(&entry)? {
            return Ok(false);
        }
        self.load(s)?;
        Ok(true)
    }
    pub(super) fn missing(&self, manifest: &Manifest) -> Result<Vec<u32>> {
        let _transaction = self.locks.transaction()?;
        manifest
            .selected
            .iter()
            .filter_map(|s| match self.contains(s) {
                Ok(true) => None,
                Ok(false) => Some(Ok(s.episode)),
                Err(e) => Some(Err(e)),
            })
            .collect()
    }
    /// One attempt per cache session; a deliberate later invocation may replace an owned marker.
    pub(super) fn reserve(&self, s: &Selected) -> Result<()> {
        let _transaction = self.locks.transaction()?;
        let key = (s.show_id, s.season);
        self.lock_season(key.0, key.1)?;
        let entry = self.entry(s)?;
        let parent = entry.parent().unwrap();
        let marker = parent.join(format!("file-{}.attempt", s.file_id));
        check_ancestors(&marker)?;
        if self.contains(s)? || exists(&entry.with_extension("partial"))? {
            return Err(fail(
                "reservation refused: complete content or protected staging exists",
            ));
        }
        let previous = exists(&marker)?;
        if previous && read(&marker, MANIFEST_CAP)? != ATTEMPT {
            return Err(fail(
                "reservation refused: malformed attempt marker; retained",
            ));
        }
        let additional = if previous { 0 } else { ATTEMPT.len() as u64 };
        self.space_for(additional, Some(key))?;
        ensure_dir(parent)?;
        if !self.attempted.borrow_mut().insert(entry) {
            return Err(fail(
                "download already attempted in this invocation; no POST replay",
            ));
        }
        if previous {
            eprintln!(
                "Warning: S{:02}E{:02} file_id={} prior POST may have been charged; one new attempt this invocation",
                s.season, s.episode, s.file_id
            );
            fs::remove_file(&marker)
                .map_err(|_| fail("owned attempt marker replacement failed"))?;
        }
        new_file(&marker, ATTEMPT)
            .map_err(|_| fail("reference download already attempted or reservation failed; no POST replay, owner review required"))?;
        self.locks.consume(key, additional)
    }
    pub(super) fn publish(&self, s: &Selected, bytes: &[u8]) -> Result<()> {
        let _transaction = self.locks.transaction()?;
        let staging = self.stage(s, bytes)?;
        self.finish_staging(s, &staging, bytes, false)
    }
    /// Persist full-response expectations before writing any raw body bytes.
    pub(super) fn stage(&self, s: &Selected, bytes: &[u8]) -> Result<PathBuf> {
        let _transaction = self.locks.transaction()?;
        let key = (s.show_id, s.season);
        self.lock_season(key.0, key.1)?;
        if bytes.len() > crate::srt::MAX_SRT_BYTES {
            return Err(fail("reference content byte cap exceeded"));
        }
        let entry = self.entry(s)?;
        if exists(&entry)? {
            return Err(fail("cache entry collision; no overwrite"));
        }
        let parent = entry.parent().unwrap();
        ensure_dir(parent)?;
        let staging = parent.join(format!("file-{}.partial", s.file_id));

        if exists(&staging)? {
            return Err(fail(
                "cache raw staging already exists; recover locally, no overwrite",
            ));
        }
        let receipt = serde_json::to_vec(&DownloadReceipt {
            version: 1,
            selected: s.clone(),
            bytes: bytes.len(),
            sha256: digest(bytes),
        })
        .map_err(|_| fail("download receipt serialization failed"))?;
        if receipt.len() > MANIFEST_CAP {
            return Err(fail("download receipt byte cap exceeded"));
        }
        self.pins.borrow_mut().insert(entry);
        let additional = bytes.len() as u64 + receipt.len() as u64;
        self.space_for(additional, Some(key))?;
        fs::create_dir(&staging)
            .map_err(|_| fail("cache partial directory collision or creation failure"))?;
        new_file(&staging.join("download.json"), &receipt)?;
        new_file(&staging.join("content.srt"), bytes)?;
        self.locks.consume(key, additional)?;
        Ok(staging)
    }
    /// Raw bytes are never discarded on parse failure. Recovery requires a matching
    /// full-body receipt or existing provenance; raw syntax alone proves nothing.
    pub(super) fn recover(&self, s: &Selected) -> Result<bool> {
        let _transaction = self.locks.transaction()?;
        if self.contains(s)? {
            return Ok(true);
        }
        let staging = self.entry(s)?.with_extension("partial");
        if !exists(&staging)? {
            return Ok(false);
        }
        self.inventory()?; // Includes unknown/non-Unicode child and reparse-point guards.
        let bytes = read(&staging.join("content.srt"), crate::srt::MAX_SRT_BYTES).map_err(|e| {
            fail(&format!(
                "raw staging content: {e}; retained, owner review required"
            ))
        })?;
        self.finish_staging(s, &staging, &bytes, true)?;
        Ok(true)
    }
    fn finish_staging(
        &self,
        s: &Selected,
        staging: &Path,
        bytes: &[u8],
        recovered: bool,
    ) -> Result<()> {
        let _transaction = self.locks.transaction()?;
        let key = (s.show_id, s.season);
        self.lock_season(key.0, key.1)?;
        let entry = self.entry(s)?;
        let marker = entry.with_extension("attempt");
        if exists(&marker)? && read(&marker, MANIFEST_CAP)? != ATTEMPT {
            return Err(fail("staged attempt marker malformed; retained"));
        }
        let provenance = staging.join("provenance.json");
        let receipt = staging.join("download.json");
        if exists(&receipt)? {
            let expected: DownloadReceipt = serde_json::from_slice(&read(&receipt, MANIFEST_CAP)?)
                .map_err(|_| fail("invalid staged download receipt; retained"))?;
            if expected.version != 1
                || expected.selected != *s
                || expected.bytes != bytes.len()
                || expected.sha256 != digest(bytes)
            {
                return Err(fail(
                    "staged full-body receipt mismatch; retained, no redownload",
                ));
            }
        } else if !exists(&provenance)? {
            return Err(fail(
                "staging has no full-body receipt/provenance; retained, owner validation required",
            ));
        }
        let prepared = transcript(bytes).map_err(|e| {
            fail(&format!(
                "raw staging parse: {e}; bytes retained; no redownload"
            ))
        })?;
        if exists(&provenance)? {
            self.load_from(s, staging)
                .map_err(|e| fail(&format!("staged provenance validation: {e}; retained")))?;
        } else {
            let metadata = Provenance {
                policy: super::POLICY,
                selected: s.clone(),
                fetched_unix_seconds: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| fail("system clock precedes epoch"))?
                    .as_secs(),
                sha256: digest(bytes),
                bytes: bytes.len(),
                format: content_format(
                    &prepared.caption_normalization,
                    &prepared.cue_ordering,
                    &prepared.cue_boundaries,
                    &prepared.zero_duration,
                    &prepared.cue_numbering,
                    &prepared.record_framing,
                    &prepared.missing_text,
                    &prepared.caption_paragraphs,
                    &prepared.source_layout,
                )
                .into(),
                recovered_without_fetch_metadata: recovered,
                caption_normalization: prepared.caption_normalization.clone(),
                cue_ordering: prepared.cue_ordering.clone(),
                cue_boundaries: prepared.cue_boundaries.clone(),
                zero_duration: prepared.zero_duration.clone(),
                cue_numbering: prepared.cue_numbering.clone(),
                record_framing: prepared.record_framing.clone(),
                missing_text: prepared.missing_text.clone(),
                caption_paragraphs: prepared.caption_paragraphs.clone(),
                source_layout: prepared.source_layout.clone(),
            };
            let json = serde_json::to_vec_pretty(&metadata)
                .map_err(|_| fail("provenance serialization failed"))?;
            self.pins.borrow_mut().insert(entry.clone());
            self.space_for(json.len() as u64, Some(key))?;
            new_file(&provenance, &json)?;
            self.locks.consume(key, json.len() as u64)?;
        }
        // Synced provenance now carries the full-body digest/size across a crash here.
        if exists(&receipt)? {
            fs::remove_file(&receipt)
                .map_err(|_| fail("staged receipt cleanup failed; retained"))?;
        }
        if let Some(p) = prepared.record_framing {
            eprintln!(
                "S{:02}E{:02} file_id={}: framed {} timed records ({} unindexed, {} missing separators, {} fractional labels, {} normalized timing spellings); raw bytes unchanged; policy={}",
                s.season,
                s.episode,
                s.file_id,
                p.original_cues,
                p.unindexed_cues,
                p.missing_separators,
                p.fractional_labels,
                p.normalized_timings,
                p.policy
            );
        }
        if let Some(policy) = prepared.source_layout {
            eprintln!(
                "Normalized source layout: {}, {} bare-CR endings, {} positioned cues ({}); caption text/times preserved; raw bytes unchanged.",
                policy.encoding, policy.bare_cr_line_endings, policy.positioned_cues, policy.policy
            );
        }
        if let Some(policy) = prepared.caption_paragraphs {
            eprintln!(
                "Normalized {} interior blank caption lines in {} cues ({}); nonblank captions/times retained; raw bytes unchanged.",
                policy.removed_blank_lines, policy.affected_cues, policy.policy
            );
        }
        if let Some(policy) = prepared.missing_text {
            eprintln!(
                "S{:02}E{:02} file_id={}: skipped {} MissingText records; {} nonempty records remain before zero-duration filtering; raw bytes unchanged; policy={}",
                s.season,
                s.episode,
                s.file_id,
                policy.skipped_cues,
                policy.retained_cues,
                policy.policy
            );
        }
        if let Some(policy) = prepared.zero_duration {
            eprintln!(
                "S{:02}E{:02} file_id={}: skipped {} zero-duration cues; {} retained; raw bytes unchanged; policy={}",
                s.season,
                s.episode,
                s.file_id,
                policy.skipped_cues,
                policy.retained_cues,
                policy.policy
            );
        }
        if let Some(policy) = prepared.cue_numbering {
            eprintln!(
                "S{:02}E{:02} file_id={}: renumbered {} of {} cue headers; caption text/timestamps/raw bytes unchanged; policy={}",
                s.season,
                s.episode,
                s.file_id,
                policy.renumbered_cues,
                policy.original_cues,
                policy.policy
            );
        }
        if let Some(policy) = prepared.cue_boundaries {
            eprintln!(
                "S{:02}E{:02} file_id={}: recovered {} unindexed timed cues; timestamps/raw bytes unchanged; policy={}",
                s.season, s.episode, s.file_id, policy.unindexed_cues, policy.policy
            );
        }
        if let Some(policy) = prepared.cue_ordering {
            eprintln!(
                "S{:02}E{:02} file_id={}: cue ordering moved {} of {} cues; timestamps/raw bytes unchanged; policy={}",
                s.season, s.episode, s.file_id, policy.moved_cues, policy.cue_count, policy.policy
            );
        }
        if let Some(policy) = prepared.caption_normalization {
            eprintln!(
                "S{:02}E{:02} file_id={}: caption derivative policy={} replacements={}; raw bytes/hash unchanged",
                s.season, s.episode, s.file_id, policy.policy, policy.replacements
            );
        }
        // The shared transaction and season ownership prevent cooperating writers racing.
        fs::rename(staging, &entry)
            .map_err(|_| fail("atomic cache publication failed; partial ignored"))?;
        Ok(())
    }
    fn load(&self, s: &Selected) -> Result<crate::srt::Transcript> {
        self.load_from(s, &self.entry(s)?)
    }
    fn load_from(&self, s: &Selected, entry: &Path) -> Result<crate::srt::Transcript> {
        check_ancestors(entry)?;
        let p: Provenance =
            serde_json::from_slice(&read(&entry.join("provenance.json"), MANIFEST_CAP)?)
                .map_err(|_| fail("invalid cache provenance"))?;
        if p.policy != super::POLICY
            || p.selected != *s
            || p.format
                != content_format(
                    &p.caption_normalization,
                    &p.cue_ordering,
                    &p.cue_boundaries,
                    &p.zero_duration,
                    &p.cue_numbering,
                    &p.record_framing,
                    &p.missing_text,
                    &p.caption_paragraphs,
                    &p.source_layout,
                )
            || p.fetched_unix_seconds == 0
        {
            return Err(fail("cached content provenance/label conflict"));
        }
        let bytes = read(&entry.join("content.srt"), crate::srt::MAX_SRT_BYTES)?;
        if bytes.len() != p.bytes || digest(&bytes) != p.sha256 {
            return Err(fail(
                "cached content digest/size mismatch; not fetching alternatives",
            ));
        }
        let prepared = transcript(&bytes)?;
        if prepared.caption_normalization != p.caption_normalization {
            return Err(fail("cached caption derivative policy/count mismatch"));
        }
        if prepared.cue_ordering != p.cue_ordering {
            return Err(fail("cached cue ordering policy/permutation mismatch"));
        }
        if prepared.cue_boundaries != p.cue_boundaries {
            return Err(fail("cached cue boundary policy/mapping mismatch"));
        }
        if prepared.zero_duration != p.zero_duration
            || prepared.cue_numbering != p.cue_numbering
            || prepared.record_framing != p.record_framing
            || prepared.missing_text != p.missing_text
            || prepared.caption_paragraphs != p.caption_paragraphs
            || prepared.source_layout != p.source_layout
        {
            return Err(fail(
                "cached record derivative policy/count/mapping mismatch",
            ));
        }
        Ok(prepared.transcript)
    }
    pub(super) fn references(&self, manifest: &Manifest) -> Result<Vec<Reference>> {
        let _transaction = self.locks.transaction()?;
        self.pin(manifest)?;
        manifest
            .selected
            .iter()
            .map(|s| {
                Reference::new(
                    ReferenceId::new("opensubtitles", &s.episode_id.to_string())
                        .map_err(|_| fail("invalid cached ID"))?,
                    &s.label(),
                    &format!(
                        "{}; English; file_id={}; selection-v{}; provider label unverified",
                        s.source,
                        s.file_id,
                        super::POLICY
                    ),
                    self.load(s)?,
                )
                .map_err(|_| fail("invalid cached reference label"))
            })
            .collect()
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadReceipt {
    version: u32,
    selected: Selected,
    bytes: usize,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    policy: u32,
    selected: Selected,
    fetched_unix_seconds: u64,
    sha256: String,
    bytes: usize,
    format: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    recovered_without_fetch_metadata: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    caption_normalization: Option<super::CaptionNormalization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cue_ordering: Option<super::ordering::CueOrdering>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cue_boundaries: Option<super::boundaries::CueBoundaries>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    zero_duration: Option<super::records::ZeroDuration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cue_numbering: Option<super::records::CueNumbering>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    record_framing: Option<super::records::RecordFraming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    missing_text: Option<super::records::MissingText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    caption_paragraphs: Option<super::records::CaptionParagraphs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_layout: Option<super::records::SourceLayout>,
}
// Independent derivative policies compose; keep legacy format precedence explicit.
#[allow(clippy::too_many_arguments)]
fn content_format(
    normalization: &Option<super::CaptionNormalization>,
    ordering: &Option<super::ordering::CueOrdering>,
    boundaries: &Option<super::boundaries::CueBoundaries>,
    zero_duration: &Option<super::records::ZeroDuration>,
    cue_numbering: &Option<super::records::CueNumbering>,
    framing: &Option<super::records::RecordFraming>,
    missing_text: &Option<super::records::MissingText>,
    caption_paragraphs: &Option<super::records::CaptionParagraphs>,
    source_layout: &Option<super::records::SourceLayout>,
) -> &'static str {
    if source_layout.is_some() {
        "srt-raw-with-source-layout-v1"
    } else if caption_paragraphs.is_some() {
        "srt-utf8-raw-with-caption-paragraphs-v1"
    } else if missing_text.is_some() {
        "srt-utf8-raw-with-missing-text-derivative-v1"
    } else if framing.is_some() {
        "srt-utf8-raw-with-record-framing-v2"
    } else if zero_duration.is_some() || cue_numbering.is_some() {
        "srt-utf8-raw-with-record-derivatives-v1"
    } else if boundaries.is_some() {
        "srt-utf8-raw-with-unindexed-cue-derivative-v1"
    } else if ordering.is_some() {
        "srt-utf8-raw-with-cue-ordering-v1"
    } else if normalization
        .as_ref()
        .is_some_and(|n| n.policy == "caption-c1-placeholders-v2")
    {
        "srt-utf8-raw-with-caption-derivative-v2"
    } else if normalization.is_some() {
        "srt-utf8-raw-with-caption-derivative-v1"
    } else {
        "srt-utf8-original"
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            refuse_link(&m)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(fail("cache metadata read failed")),
    }
}
fn refuse_link(m: &fs::Metadata) -> Result<()> {
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
    if m.file_type().is_symlink() || reparse {
        return Err(fail("cache symlink/reparse point refused"));
    }
    Ok(())
}
fn check_ancestors(path: &Path) -> Result<()> {
    for part in path.components() {
        if matches!(
            part,
            std::path::Component::ParentDir | std::path::Component::CurDir
        ) {
            return Err(fail("cache path traversal refused"));
        }
    }
    for ancestor in path.ancestors() {
        if !ancestor.as_os_str().is_empty() {
            exists(ancestor)?;
        }
    }
    Ok(())
}
fn ensure_dir(path: &Path) -> Result<()> {
    check_ancestors(path)?;
    if exists(path)? {
        if !path.is_dir() {
            return Err(fail("cache directory path collision"));
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let created = match fs::create_dir(path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            check_ancestors(path)?;
            if !path.is_dir() {
                return Err(fail("cache directory path collision"));
            }
            false
        }
        Err(_) => return Err(fail("private cache directory creation failed")),
    };
    #[cfg(not(unix))]
    let _ = created;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if created {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|_| fail("private cache permissions failed"))?;
        }
    }
    Ok(())
}
fn read(path: &Path, cap: usize) -> Result<Vec<u8>> {
    check_ancestors(path)?;
    let m = fs::symlink_metadata(path).map_err(|_| fail("cache file missing/unreadable"))?;
    if !m.is_file() {
        return Err(fail("cache entry is not regular file"));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| fail("cache open failed"))?
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| fail("cache read failed"))?;
    if bytes.len() > cap {
        return Err(fail("cache byte cap exceeded"));
    }
    Ok(bytes)
}
fn new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    check_ancestors(path)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| fail("cache file collision or creation failed"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| fail("cache write/sync failed; partial ignored"))
}
fn atomic_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if exists(path)? {
        return Err(fail("frozen manifest collision; no overwrite"));
    }
    let partial = path.with_extension("partial");
    new_file(&partial, bytes)?;
    fs::rename(partial, path).map_err(|_| fail("atomic manifest publication failed"))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "opt-in read-only retained-cache/parser compatibility audit"]
    fn inspect_retained_parser_compatibility() {
        let Some(root) = std::env::var_os("TVMATCH_TEST_CACHE_AUDIT") else {
            return;
        };
        fn visit(path: &std::path::Path, entries: &mut usize, count: &mut usize) {
            super::check_ancestors(path).unwrap();
            for item in std::fs::read_dir(path).unwrap() {
                *entries += 1;
                assert!(*entries <= 20_000);
                let path = item.unwrap().path();
                let meta = std::fs::symlink_metadata(&path).unwrap();
                assert!(!meta.file_type().is_symlink());
                if meta.is_dir() {
                    visit(&path, entries, count);
                    continue;
                }
                if path.file_name() != Some(std::ffi::OsStr::new("provenance.json")) {
                    continue;
                }
                let metadata = super::read(&path, super::MANIFEST_CAP).unwrap();
                let p: super::Provenance = serde_json::from_slice(&metadata).unwrap();
                let body_path = path.with_file_name("content.srt");
                let raw = super::read(&body_path, crate::srt::MAX_SRT_BYTES).unwrap();
                assert_eq!(raw.len(), p.bytes);
                assert_eq!(super::digest(&raw), p.sha256);
                let prepared = super::transcript(&raw).unwrap();
                assert_eq!(prepared.caption_normalization, p.caption_normalization);
                assert_eq!(prepared.cue_ordering, p.cue_ordering);
                assert_eq!(prepared.cue_boundaries, p.cue_boundaries);
                assert_eq!(prepared.zero_duration, p.zero_duration);
                assert_eq!(prepared.cue_numbering, p.cue_numbering);
                assert_eq!(prepared.record_framing, p.record_framing);
                assert_eq!(prepared.missing_text, p.missing_text);
                assert_eq!(prepared.caption_paragraphs, p.caption_paragraphs);
                assert_eq!(prepared.source_layout, p.source_layout);
                assert!(metadata == super::read(&path, super::MANIFEST_CAP).unwrap());
                assert!(raw == super::read(&body_path, crate::srt::MAX_SRT_BYTES).unwrap());
                *count += 1;
            }
        }
        let root = std::path::PathBuf::from(root);
        let mut count = 0;
        visit(&root, &mut 0, &mut count);
        assert!(count > 0);
        println!(
            "{count} published references recomputed with exactly matching stored policies; no cache writes or HTTP"
        );
    }
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    pub(crate) fn temp() -> PathBuf {
        std::env::temp_dir().join(format!(
            "tvmatch-cache-test-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SERIAL.fetch_add(1, Ordering::SeqCst)
        ))
    }
    pub(crate) fn selected() -> Selected {
        Selected {
            show_id: 1,
            episode_id: 2,
            season: 1,
            episode: 1,
            title: "Original Episode".into(),
            subtitle_id: 3,
            file_id: 4,
            language: "en".into(),
            release: "Original.S01E01".into(),
            uploader: "Original".into(),
            source: "https://www.opensubtitles.com/en/subtitles/3".into(),
        }
    }
    #[test]
    fn fallback_history_counts_against_capacity_and_protects_successful_replacements() {
        let root = temp();
        let mut c = Cache::open(&root).unwrap();
        let origin = selected();
        c.reserve(&origin).unwrap();
        c.stage(&origin, b"1\n00:00:01,000 --> 00:00:02,000\n\n")
            .unwrap();
        let proof = c.empty_proof(&origin).unwrap().unwrap();
        let mut candidate = origin.clone();
        candidate.file_id = 5;
        candidate.subtitle_id = 6;
        candidate.source = "https://www.opensubtitles.com/en/subtitles/6".into();
        let mut m = Manifest {
            show_choice: None,
            policy: 1,
            scope: Scope::new("Original Show", "1", None).unwrap(),
            show_id: 1,
            show_title: "Original Show".into(),
            selected: vec![origin.clone()],
            unavailable: vec![],
        };
        c.freeze(&m).unwrap();
        m.selected[0] = candidate.clone();
        let before = c.inventory().unwrap().total;
        c.cap = before + 10;
        assert!(c.prepare(&m).is_err());
        assert!(
            c.approve_fallback(&origin, &origin, &candidate, &proof)
                .is_err()
        );
        assert!(
            !c.entry(&candidate)
                .unwrap()
                .with_extension("attempt")
                .exists()
        );
        c.cap = CACHE_CAP;
        c.prepare(&m).unwrap();
        c.approve_fallback(&origin, &origin, &candidate, &proof)
            .unwrap();
        assert!(c.inventory().unwrap().total > before);
        c.reserve(&candidate).unwrap();
        c.publish(&candidate, SRT).unwrap();
        let total = c.inventory().unwrap().total;
        let path = c.entry(&candidate).unwrap();
        drop(c);
        let c = Cache::open_with_cap(&root, total).unwrap();
        assert!(c.space(1).is_err());
        assert!(path.join("content.srt").exists());
        assert_eq!(fs::read(path.join("content.srt")).unwrap(), SRT);
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn series_display_metadata_counts_against_capacity_and_never_overwrites_existing_state() {
        let root = temp();
        let m = Manifest {
            show_choice: None,
            policy: 1,
            scope: Scope::new("Original Show", "1", None).unwrap(),
            show_id: 1,
            show_title: "Original Show".into(),
            selected: vec![selected()],
            unavailable: vec![],
        };
        let d = crate::opensubtitles::series::Display {
            policy: "provider-series-display-v1".into(),
            show_id: 1,
            imdb_id: None,
            original_title: Some("Original Show".into()),
            year: 2000,
        };
        let c = Cache::open_with_cap(&root, 1).unwrap();
        assert!(c.store_display(&m, &d).is_err());
        assert!(!root.join("series-1.json").exists());
        drop(c);
        let c = Cache::open(&root).unwrap();
        let before = c.inventory().unwrap().total;
        c.store_display(&m, &d).unwrap();
        let bytes = fs::read(root.join("series-1.json")).unwrap();
        assert_eq!(c.inventory().unwrap().total, before + bytes.len() as u64);
        let mut changed = d.clone();
        changed.year = 2001;
        assert!(c.store_display(&m, &changed).is_err());
        assert_eq!(fs::read(root.join("series-1.json")).unwrap(), bytes);
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    const SRT: &[u8] =
        b"1\r\n00:00:01,000 --> 00:00:02,000\r\nOriginal synthetic dialogue only.\r\n";
    #[test]
    fn atomic_contents_digest_collision_and_lock() {
        let root = temp();
        let c = Cache::open(&root).unwrap();
        let s = selected();
        let concurrent = Cache::open(&root).unwrap();
        assert!(!c.contains(&s).unwrap());
        assert!(concurrent.contains(&s).is_err());
        drop(concurrent);
        c.reserve(&s).unwrap();
        assert!(c.reserve(&s).is_err());
        c.publish(&s, SRT).unwrap();
        assert!(c.contains(&s).unwrap());
        assert!(c.publish(&s, SRT).is_err());
        let path = c.entry(&s).unwrap().join("content.srt");
        assert_eq!(fs::read(&path).unwrap(), SRT);
        fs::write(path, b"corrupt").unwrap();
        assert!(c.contains(&s).is_err());
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn non_utf8_html_caps_partial_and_labels() {
        let root = temp();
        let c = Cache::open(&root).unwrap();
        let mut s = selected();
        for bad in [vec![255], b"<html>not subtitles</html>".to_vec()] {
            assert!(c.publish(&s, &bad).is_err());
            assert!(!c.contains(&s).unwrap());
            assert_eq!(
                read(
                    &c.entry(&s)
                        .unwrap()
                        .with_extension("partial")
                        .join("content.srt"),
                    crate::srt::MAX_SRT_BYTES
                )
                .unwrap(),
                bad
            );
            s.file_id += 1;
        }
        assert!(
            c.publish(&s, &vec![b'a'; crate::srt::MAX_SRT_BYTES + 1])
                .is_err()
        );
        assert!(!c.entry(&s).unwrap().with_extension("partial").exists());
        s.file_id = 4;
        let parent = c.entry(&s).unwrap().parent().unwrap().to_owned();
        ensure_dir(&parent).unwrap();
        assert_eq!(
            fs::read(parent.join("file-4.partial/content.srt")).unwrap(),
            vec![255]
        );
        assert!(!c.contains(&s).unwrap());
        assert!(c.publish(&s, SRT).is_err());
        s.language = "fr".into();
        assert!(c.contains(&s).is_err());
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn traversal_and_regular_path_collision() {
        let root = temp();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("collision"), b"x").unwrap();
        assert!(Cache::open(&root.join("collision")).is_err());
        assert!(Cache::open(&root.join("..").join("escape")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn scope_and_manifest_duplicate_poison() {
        for (s, r) in [
            ("0", Some("1-8")),
            ("1", Some("8-1")),
            ("1", Some("1-1001")),
            ("1", Some("0")),
            ("1", Some("+1")),
            ("1", Some("1-2-3")),
        ] {
            assert!(Scope::new("Original Show", s, r).is_err());
        }
        let scope = Scope::new("Original Show", "1", Some("1")).unwrap();
        let mut m = Manifest {
            show_choice: None,
            policy: super::super::POLICY,
            scope: scope.clone(),
            show_id: 1,
            show_title: scope.show.clone(),
            unavailable: Vec::new(),
            selected: vec![selected()],
        };
        assert!(m.validate(&scope).is_ok());
        m.selected.push(selected());
        assert!(m.validate(&scope).is_err());
    }
    #[test]
    fn series_display_name_uses_verified_cache_for_name_and_imdb_scopes() {
        let root = temp();
        let c = Cache::open(&root).unwrap();
        let imdb = Scope::from_imdb("tt123", "1", Some("1")).unwrap();
        assert!(c.series_name(&imdb).is_err());
        let mut m = Manifest {
            show_choice: None,
            policy: super::super::POLICY,
            scope: imdb.clone(),
            show_id: 1,
            show_title: "original show".into(),
            unavailable: Vec::new(),
            selected: vec![selected()],
        };
        c.freeze(&m).unwrap();
        assert_eq!(c.series_name(&imdb).unwrap(), "original show");
        m.scope = Scope::new("Original Show", "1", Some("1")).unwrap();
        c.freeze(&m).unwrap();
        assert_eq!(c.series_name(&m.scope).unwrap(), "Original Show");
        assert_eq!(c.series_name(&imdb).unwrap(), "Original Show");
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    fn insert(c: &Cache, id: u64, time: u64) -> Selected {
        let mut s = selected();
        s.file_id = id;
        s.episode_id = id;
        s.episode = id as u32;
        c.reserve(&s).unwrap();
        c.publish(&s, SRT).unwrap();
        let path = c.entry(&s).unwrap().join("provenance.json");
        let mut p: Provenance = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        p.fetched_unix_seconds = time;
        fs::write(path, serde_json::to_vec_pretty(&p).unwrap()).unwrap();
        c.pins.borrow_mut().clear();
        s
    }
    #[test]
    fn fifo_exact_boundary_multi_eviction_read_does_not_refresh_and_refetch() {
        let root = temp();
        let mut c = Cache::open(&root).unwrap();
        let a = insert(&c, 4, 1);
        let b = insert(&c, 5, 2);
        let d = insert(&c, 6, 3);
        assert!(c.contains(&a).unwrap());
        let inv = c.inventory().unwrap();
        c.cap = inv.total;
        c.space(0).unwrap();
        assert!(c.contains(&a).unwrap());
        let first_two = inv.entries[0].2 + inv.entries[1].2;
        c.space(first_two).unwrap();
        assert!(!c.contains(&a).unwrap());
        assert!(!c.contains(&b).unwrap());
        assert!(c.contains(&d).unwrap());
        assert!(!c.entry(&a).unwrap().with_extension("attempt").exists());
        let cap = c.cap;
        drop(c);
        let c = Cache::open_with_cap(&root, cap).unwrap();
        c.reserve(&a).unwrap();
        c.publish(&a, SRT).unwrap();
        assert!(c.contains(&a).unwrap());
        assert!(c.inventory().unwrap().total <= c.cap);
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn startup_trim_protected_attempt_staging_and_active_set() {
        let root = temp();
        let mut c = Cache::open(&root).unwrap();
        let a = insert(&c, 4, 1);
        let b = insert(&c, 5, 2);
        let size = c.inventory().unwrap().entries[1].2;
        c.pins.borrow_mut().insert(c.entry(&a).unwrap());
        c.cap = size;
        c.space(0).unwrap();
        assert!(c.contains(&a).unwrap());
        assert!(!c.contains(&b).unwrap());
        c.reserve(&b).unwrap_err(); // pinned content + marker cannot fit
        c.pins.borrow_mut().clear();
        drop(c);
        let c = Cache::open(&root).unwrap();
        c.reserve(&b).unwrap();
        drop(c);
        let c = Cache::open_with_cap(&root, ATTEMPT.len() as u64).unwrap();
        assert!(!c.contains(&a).unwrap());
        assert!(!c.contains(&b).unwrap());
        assert!(c.entry(&b).unwrap().with_extension("attempt").exists());
        c.reserve(&b).unwrap(); // deliberate new invocation may replace only its owned marker
        assert!(c.reserve(&b).is_err());
        drop(c);
        assert!(Cache::open_with_cap(&root, ATTEMPT.len() as u64 - 1).is_err());
        assert!(root.join(".lock").exists()); // Persistent fence is not a held OS lock.
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn metadata_staging_accounted_and_impossible_working_set_fails_before_reservation() {
        let root = temp();
        let mut c = Cache::open(&root).unwrap();
        let scope = Scope::new("Original Show", "1", Some("1")).unwrap();
        let m = Manifest {
            show_choice: None,
            policy: super::super::POLICY,
            scope,
            show_id: 1,
            show_title: "Original Show".into(),
            unavailable: Vec::new(),
            selected: vec![selected()],
        };
        c.freeze(&m).unwrap();
        let total = c.inventory().unwrap().total;
        assert_eq!(
            total,
            fs::metadata(c.request_path(&m.scope).unwrap())
                .unwrap()
                .len()
        );
        c.cap = total;
        assert!(c.prepare(&m).is_err());
        assert!(
            !c.entry(&m.selected[0])
                .unwrap()
                .with_extension("attempt")
                .exists()
        );
        c.pins.borrow_mut().clear();
        c.space(1).unwrap();
        assert!(c.manifest(&m.scope).unwrap().is_none());
        let staging = c.entry(&m.selected[0]).unwrap().with_extension("partial");
        ensure_dir(&staging).unwrap();
        new_file(&staging.join("content.srt"), b"partial").unwrap();
        assert_eq!(c.inventory().unwrap().total, 7);
        c.cap = 6;
        assert!(c.space(0).is_err());
        assert!(staging.exists());
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn malformed_owned_content_and_unowned_paths_refused_or_preserved() {
        let root = temp();
        let c = Cache::open(&root).unwrap();
        fs::write(root.join("user-data"), b"untouched").unwrap();
        assert_eq!(c.inventory().unwrap().total, 0);
        let s = insert(&c, 4, 1);
        fs::write(c.entry(&s).unwrap().join("provenance.json"), b"{}").unwrap();
        assert!(c.space(0).is_err());
        assert!(root.join("user-data").exists());
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(any(unix, windows))]
    #[test]
    fn non_unicode_entry_child_refuses_eviction_without_deleting_owned_files() {
        #[cfg(unix)]
        let name = {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(vec![0xff])
        };
        #[cfg(windows)]
        let name = {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[0xd800])
        };
        assert!(name.to_str().is_none());
        let root = temp();
        let mut c = Cache::open(&root).unwrap();
        // Unowned names outside a content entry remain untouched and ignored.
        fs::write(root.join(&name), b"unowned root file").unwrap();
        assert_eq!(c.inventory().unwrap().total, 0);
        let s = insert(&c, 4, 1);
        let entry = c.entry(&s).unwrap();
        let provenance = fs::read(entry.join("provenance.json")).unwrap();
        let marker = fs::read(entry.with_extension("attempt")).unwrap();
        c.cap = c.inventory().unwrap().total;
        fs::write(entry.join(&name), b"unowned entry file").unwrap();
        assert!(c.space(1).is_err());
        assert_eq!(fs::read(entry.join("content.srt")).unwrap(), SRT);
        assert_eq!(fs::read(entry.join("provenance.json")).unwrap(), provenance);
        assert_eq!(fs::read(entry.with_extension("attempt")).unwrap(), marker);
        assert_eq!(fs::read(entry.join(&name)).unwrap(), b"unowned entry file");
        assert_eq!(fs::read(root.join(&name)).unwrap(), b"unowned root file");
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn symlink_cache_tree_is_never_followed() {
        let root = temp();
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(std::env::temp_dir(), root.join("show-1")).unwrap();
        assert!(Cache::open(&root).is_err());
        fs::remove_file(root.join("show-1")).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn frozen_active_content_and_uncertain_manifest_are_not_capacity_victims() {
        let root = temp();
        let mut c = Cache::open(&root).unwrap();
        let s = insert(&c, 4, 1);
        let scope = Scope::new("Original Show", "1", Some("4")).unwrap();
        let m = Manifest {
            show_choice: None,
            policy: super::super::POLICY,
            scope,
            show_id: 1,
            show_title: "Original Show".into(),
            selected: vec![s.clone()],
            unavailable: Vec::new(),
        };
        c.cap = c.inventory().unwrap().total;
        assert!(c.freeze(&m).is_err());
        assert!(c.contains(&s).unwrap());
        assert!(c.entry(&s).unwrap().with_extension("attempt").exists());
        c.cap = CACHE_CAP;
        c.freeze(&m).unwrap();
        // Simulate uncertain missing content without clearing its durable charge record.
        let entry = c.entry(&s).unwrap();
        fs::remove_file(entry.join("content.srt")).unwrap();
        fs::remove_file(entry.join("provenance.json")).unwrap();
        fs::remove_dir(entry).unwrap();
        c.pins.borrow_mut().clear();
        c.cap = ATTEMPT.len() as u64;
        assert!(c.space(0).is_err());
        assert!(c.manifest(&m.scope).unwrap().is_some());
        assert!(c.prepare(&m).is_err());
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn staging_provenance_conflict_and_unknown_child_are_retained() {
        for unknown in [false, true] {
            let root = temp();
            let c = Cache::open(&root).unwrap();
            let s = selected();
            c.publish(&s, SRT).unwrap();
            let entry = c.entry(&s).unwrap();
            let partial = entry.with_extension("partial");
            fs::rename(&entry, &partial).unwrap();
            if unknown {
                fs::write(partial.join("user-data"), b"untouched").unwrap();
            } else {
                let path = partial.join("provenance.json");
                let mut p: Provenance = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                p.selected.episode_id += 1;
                fs::write(path, serde_json::to_vec(&p).unwrap()).unwrap();
            }
            assert!(c.recover(&s).is_err());
            assert!(c.reserve(&s).is_err());
            assert_eq!(fs::read(partial.join("content.srt")).unwrap(), SRT);
            assert!(!entry.exists());
            drop(c);
            fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn raw_only_valid_prefix_is_not_completion_evidence() {
        let root = temp();
        let c = Cache::open(&root).unwrap();
        let s = selected();
        c.reserve(&s).unwrap();
        let partial = c.entry(&s).unwrap().with_extension("partial");
        ensure_dir(&partial).unwrap();
        // A failed write can leave a complete first cue of a longer response.
        new_file(&partial.join("content.srt"), SRT).unwrap();
        drop(c);
        let c = Cache::open(&root).unwrap();
        assert!(c.recover(&s).is_err());
        assert!(!c.contains(&s).unwrap());
        assert_eq!(fs::read(partial.join("content.srt")).unwrap(), SRT);
        assert!(partial.with_extension("attempt").exists());
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn staged_recovery_refuses_malformed_sibling_marker_without_mutation() {
        let root = temp();
        let c = Cache::open(&root).unwrap();
        let s = selected();
        c.publish(&s, SRT).unwrap();
        let entry = c.entry(&s).unwrap();
        let partial = entry.with_extension("partial");
        fs::rename(&entry, &partial).unwrap();
        let provenance = fs::read(partial.join("provenance.json")).unwrap();
        fs::write(entry.with_extension("attempt"), b"not an owned marker").unwrap();
        assert!(c.recover(&s).is_err());
        assert!(!entry.exists());
        assert_eq!(fs::read(partial.join("content.srt")).unwrap(), SRT);
        assert_eq!(
            fs::read(partial.join("provenance.json")).unwrap(),
            provenance
        );
        assert_eq!(
            fs::read(entry.with_extension("attempt")).unwrap(),
            b"not an owned marker"
        );
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn raw_recovery_pins_entire_active_set_before_metadata_space() {
        let root = temp();
        let mut c = Cache::open(&root).unwrap();
        let a = insert(&c, 4, 1);
        let mut b = selected();
        b.episode = 5;
        b.episode_id = 5;
        b.file_id = 5;
        let partial = c.stage(&b, SRT).unwrap();
        let m = Manifest {
            show_choice: None,
            policy: super::super::POLICY,
            scope: Scope::new("Original Show", "1", None).unwrap(),
            show_id: 1,
            show_title: "Original Show".into(),
            selected: vec![a.clone(), b],
            unavailable: Vec::new(),
        };
        c.cap = c.inventory().unwrap().total;
        let (_, failures) = super::super::recover_missing(&c, &m).unwrap();
        assert_eq!(failures.len(), 1);
        assert!(c.contains(&a).unwrap());
        assert_eq!(fs::read(partial.join("content.srt")).unwrap(), SRT);
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
}
