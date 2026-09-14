//! Ephemeral, Identified-only rename plans. No overwrite fallback or rollback.
use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};
use tvmatch::MatchOutcome;
mod native;

pub(crate) fn text(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
            out.push_str(&format!("⟦U+{:04X}⟧", c as u32));
        } else {
            out.push(c);
            if c == '⟦' {
                out.push(c);
            }
        }
    }
    out
}
pub(crate) fn name(s: &OsStr) -> String {
    if let Some(s) = s.to_str() {
        return text(s);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = s.as_bytes();
        let mut out = String::new();
        while !bytes.is_empty() {
            match std::str::from_utf8(bytes) {
                Ok(s) => {
                    out.push_str(&text(s));
                    break;
                }
                Err(e) => {
                    out.push_str(&text(
                        std::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap(),
                    ));
                    let end =
                        e.valid_up_to() + e.error_len().unwrap_or(bytes.len() - e.valid_up_to());
                    for b in &bytes[e.valid_up_to()..end] {
                        out.push_str(&format!("⟦byte:{b:02X}⟧"));
                    }
                    bytes = &bytes[end..];
                }
            }
        }
        out
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        char::decode_utf16(s.encode_wide())
            .map(|c| match c {
                Ok(c) => text(&c.to_string()),
                Err(e) => format!("⟦unit:{:04X}⟧", e.unpaired_surrogate()),
            })
            .collect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        text(&s.to_string_lossy())
    }
}
fn linked(m: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    m.file_type().is_symlink()
}
pub(crate) fn check_folder(path: &Path) -> io::Result<()> {
    for p in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
        let m = fs::symlink_metadata(p)?;
        if linked(&m) || !m.is_dir() {
            return Err(io::Error::other(
                "folder/ancestors must be real directories, not links or reparse points",
            ));
        }
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    len: u64,
    modified: SystemTime,
    identity: (u64, u64),
    extra: (u64, u64),
}
impl Snapshot {
    pub(crate) fn take(path: &Path) -> io::Result<Self> {
        check_folder(
            path.parent()
                .ok_or_else(|| io::Error::other("source has no folder"))?,
        )?;
        let m = fs::symlink_metadata(path)?;
        if linked(&m) || !m.is_file() {
            return Err(io::Error::other(
                "source must be a regular file, not link or reparse point",
            ));
        }
        let file = fs::File::open(path)?;
        let m = file.metadata()?;
        let (identity, extra) = native::identity(&file, &m)?;
        Ok(Self {
            len: m.len(),
            modified: m.modified()?,
            identity,
            extra,
        })
    }
    pub(crate) fn verify(&self, path: &Path) -> io::Result<()> {
        if Self::take(path)? != *self {
            return Err(io::Error::other("source changed since scanning"));
        }
        Ok(())
    }
}
pub(crate) struct Scan {
    pub series: String,
    pub path: PathBuf,
    pub snapshot: Option<Snapshot>,
    pub outcome: Result<MatchOutcome, String>,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum State {
    Planned,
    AlreadyCorrect,
    Conflict(String),
    Applied,
    Failed(String),
    Untouched,
}
pub(crate) struct Entry {
    pub scan: Scan,
    pub target: Option<PathBuf>,
    pub state: State,
}
pub(crate) struct ExpectedEpisode {
    pub season: u32,
    pub number: u32,
    pub references: Vec<tvmatch::ReferenceId>,
}
pub(crate) struct Plan {
    pub entries: Vec<Entry>,
    expected: Vec<ExpectedEpisode>,
}

// This parser accepts only the provider's canonical cached label, never a media filename.
fn canonical(outcome: &MatchOutcome) -> Result<(&str, &str), &'static str> {
    let MatchOutcome::Identified { best, .. } = outcome else {
        return Err("not identified");
    };
    if best.reference.id.namespace() != "opensubtitles" {
        return Err("no canonical provider episode label");
    }
    let (code, title) = best
        .reference
        .display_name
        .split_once(' ')
        .ok_or("missing canonical title")?;
    let (s, e) = code
        .strip_prefix('S')
        .and_then(|s| s.split_once('E'))
        .ok_or("invalid canonical episode code")?;
    let number = |s: &str, max: u32| {
        s.parse::<u32>()
            .ok()
            .filter(|n| *n > 0 && *n <= max && format!("{n:02}") == s)
    };
    if number(s, 100).is_none() || number(e, 1000).is_none() || title.trim().is_empty() {
        return Err("invalid canonical episode label");
    }
    Ok((code, title))
}
fn safe_component(value: &str) -> String {
    let value: String = value
        .chars()
        .map(|c| {
            if c.is_control()
                || "<>:\"/\\|?*".contains(c)
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    value.trim().trim_end_matches(['.', ' ']).to_owned()
}
fn basename(outcome: &MatchOutcome, source: &Path, series: &str) -> Result<String, &'static str> {
    let (code, title) = canonical(outcome)?;
    if series.len() > 207 {
        // 200-byte name plus " (YYYY)".
        return Err("series prefix exceeds metadata bound");
    }
    let series = safe_component(series);
    let title = safe_component(title);
    if series.is_empty() || title.is_empty() {
        return Err("series or title empty after sanitization");
    }
    let extension = source
        .extension()
        .and_then(OsStr::to_str)
        .filter(|s| {
            ["mkv", "mp4", "m4v"]
                .iter()
                .any(|ext| s.eq_ignore_ascii_case(ext))
        })
        .ok_or("unsupported native extension")?;
    let mut base = format!("{series} - {code} - {title}");
    // 240 UTF-8 bytes also bounds UTF-16 units; truncate only on a Unicode scalar boundary.
    let limit = 240 - extension.len() - 1;
    while base.len() > limit {
        base.pop();
    }
    let base = base.trim_end_matches(['.', ' ']);
    Ok(format!("{base}.{extension}"))
}
fn collision_key(s: &OsStr) -> Option<String> {
    s.to_str()
        .map(|s| s.trim_end_matches(['.', ' ']).to_uppercase())
}
impl Plan {
    pub(crate) fn build(scans: Vec<Scan>) -> Self {
        let mut entries: Vec<_> = scans
            .into_iter()
            .map(|scan| {
                let mut target = None;
                let state = match &scan.outcome {
                    Ok(outcome @ MatchOutcome::Identified { .. }) => {
                        match basename(outcome, &scan.path, &scan.series) {
                            Ok(base) => {
                                target = Some(scan.path.with_file_name(base));
                                if scan.snapshot.is_none() {
                                    State::Conflict("missing source snapshot".into())
                                } else if target.as_ref() == Some(&scan.path) {
                                    State::AlreadyCorrect
                                } else {
                                    State::Planned
                                }
                            }
                            Err(e) => State::Conflict(e.into()),
                        }
                    }
                    _ => State::Untouched,
                };
                Entry {
                    scan,
                    target,
                    state,
                }
            })
            .collect();
        for i in 0..entries.len() {
            let Some(target) = entries[i].target.as_ref() else {
                continue;
            };
            if entries
                .iter()
                .enumerate()
                .any(|(j, e)| j != i && e.scan.path == entries[i].scan.path)
            {
                entries[i].state = State::Conflict("duplicate source path".into());
                continue;
            }
            let key = collision_key(target.file_name().unwrap());
            let duplicate = entries.iter().enumerate().any(|(j, e)| {
                j != i
                    && e.target.as_ref().is_some_and(|p| {
                        p.parent() == target.parent()
                            && collision_key(p.file_name().unwrap()) == key
                    })
            });
            if duplicate {
                entries[i].state =
                    State::Conflict("duplicate or case-colliding destination".into());
                continue;
            }
            if entries[i].state != State::Planned {
                continue;
            }
            if let Err(e) = entries[i]
                .scan
                .snapshot
                .as_ref()
                .unwrap()
                .verify(&entries[i].scan.path)
            {
                entries[i].state = State::Conflict(e.to_string());
            }
        }
        let locations = entries
            .iter()
            .map(|e| e.scan.path.clone())
            .collect::<Vec<_>>();
        let mut plan = Self {
            entries,
            expected: Vec::new(),
        };
        plan.propagate_blocked(&locations, false);
        plan
    }
    pub(crate) fn with_expected_episodes(mut self, expected: Vec<ExpectedEpisode>) -> Self {
        self.expected = expected;
        self
    }
    fn coverage(&self) -> String {
        if self.expected.is_empty() {
            return String::new();
        }
        let found = self
            .entries
            .iter()
            .filter_map(|e| match &e.scan.outcome {
                Ok(MatchOutcome::Identified { best, .. }) => Some(best.reference.id.clone()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        let missing = self
            .expected
            .iter()
            .filter(|e| !e.references.iter().any(|id| found.contains(id)))
            .collect::<Vec<_>>();
        let identified = self.expected.len() - missing.len();
        if missing.is_empty() {
            format!(
                "Episode coverage: {identified}/{} confidently identified; no missing matches.\n",
                self.expected.len()
            )
        } else {
            let codes = missing
                .iter()
                .map(|e| format!("S{:02}E{:02}", e.season, e.number))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Episode coverage: {identified}/{} confidently identified.\nMissing matches: {codes}\nThese episodes may be present in unidentified or unsampled content.\n",
                self.expected.len()
            )
        }
    }
    fn dependency(&self, i: usize, locations: &[PathBuf]) -> io::Result<Option<usize>> {
        let Some(occupant) = destination_occupant(self.entries[i].target.as_ref().unwrap())? else {
            return Ok(None);
        };
        let mut owners = locations
            .iter()
            .enumerate()
            .filter(|(_, path)| **path == occupant);
        if let Some((owner, _)) = owners.next()
            && owners.next().is_none()
        {
            let entry = &self.entries[owner];
            if entry.state == State::Planned {
                return Ok(Some(owner));
            }
            let file = name(occupant.file_name().unwrap());
            let reason = match &entry.state {
                State::Conflict(reason) | State::Failed(reason) => {
                    format!("blocked by {file}, whose rename cannot proceed: {reason}")
                }
                State::AlreadyCorrect => {
                    format!("destination held by already-correct file: {file}")
                }
                _ => match &entry.scan.outcome {
                    Ok(MatchOutcome::Unknown { .. }) => {
                        format!("destination held by unidentified file: {file}")
                    }
                    Ok(MatchOutcome::Ambiguous { .. }) => {
                        format!("destination held by ambiguous file: {file}")
                    }
                    Err(_) => format!("destination held by file with a scan error: {file}"),
                    _ => format!("destination held by file not moving away: {file}"),
                },
            };
            return Err(io::Error::other(reason));
        }
        Err(io::Error::other(format!(
            "destination already exists outside this rename plan: {}",
            name(occupant.file_name().unwrap())
        )))
    }
    fn propagate_blocked(&mut self, locations: &[PathBuf], applying: bool) {
        // A blocked occupant also blocks every rename waiting for its old name.
        loop {
            let mut changed = false;
            for i in 0..self.entries.len() {
                if self.entries[i].state == State::Planned
                    && let Err(error) = self.dependency(i, locations)
                {
                    self.entries[i].state = if applying {
                        State::Failed(error.to_string())
                    } else {
                        State::Conflict(error.to_string())
                    };
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
    pub(crate) fn preview(&self) -> String {
        let mut out = String::from("Rename preview:\n");
        for entry in &self.entries {
            let old = name(entry.scan.path.file_name().unwrap());
            match &entry.state {
                State::Planned => out.push_str(&format!(
                    "  📝 {old}\n    → {}\n",
                    name(entry.target.as_ref().unwrap().file_name().unwrap())
                )),
                State::AlreadyCorrect => out.push_str(&format!("  ✅ {old}: already correct\n")),
                State::Conflict(e) => {
                    out.push_str(&format!("  ⛔ {old}: conflict — {}\n", text(e)))
                }
                State::Untouched => match &entry.scan.outcome {
                    Ok(MatchOutcome::Unknown { .. }) => {
                        out.push_str(&format!("  ❓ {old}: unidentified; left untouched\n"))
                    }
                    Ok(MatchOutcome::Ambiguous { .. }) => {
                        out.push_str(&format!("  ⚠️ {old}: ambiguous; left untouched\n"))
                    }
                    Err(_) => out.push_str(&format!("  ❌ {old}: scan failed; left untouched\n")),
                    _ => (),
                },
                _ => (),
            }
        }
        let locations = self
            .entries
            .iter()
            .map(|e| e.scan.path.clone())
            .collect::<Vec<_>>();
        if self.entries.iter().enumerate().any(|(i, e)| {
            e.state == State::Planned && self.dependency(i, &locations).ok().flatten().is_some()
        }) {
            out.push_str(
                "  Occupied destinations will be freed by this plan; swaps use temporary names.\n",
            );
        }
        out.push_str(&self.coverage());
        out
    }
    pub(crate) fn dry_run(&self, output: &mut impl io::Write) -> io::Result<u8> {
        output.write_all(self.preview().as_bytes())?;
        output.write_all("Dry run; no files renamed.\n".as_bytes())?;
        let (summary, code) = self.summary(false);
        output.write_all(summary.as_bytes())?;
        Ok(code)
    }
    pub(crate) fn finish(
        &mut self,
        input: &mut impl io::Read,
        output: &mut impl io::Write,
    ) -> io::Result<u8> {
        output.write_all(self.preview().as_bytes())?;
        let mut approved = false;
        if self.entries.iter().any(|e| e.state == State::Planned) {
            output.write_all(b"Apply renames? (y/N): ")?;
            output.flush()?;
            approved = confirm(input)?;
            output.write_all(b"\n")?;
            if approved {
                self.apply();
            } else {
                output.write_all("Declined; no files renamed.\n".as_bytes())?;
            }
        }
        let (summary, code) = self.summary(approved);
        output.write_all(summary.as_bytes())?;
        Ok(code)
    }
    pub(crate) fn apply(&mut self) {
        self.apply_with(native::no_replace);
    }
    fn apply_with(&mut self, mut move_file: impl FnMut(&Path, &Path) -> io::Result<()>) {
        let mut locations = self
            .entries
            .iter()
            .map(|e| e.scan.path.clone())
            .collect::<Vec<_>>();
        let mut snapshots = self
            .entries
            .iter()
            .map(|e| e.scan.snapshot.clone())
            .collect::<Vec<_>>();
        let mut staged = vec![false; self.entries.len()];
        // Revalidate every source after the prompt, before moving any file.
        for entry in &mut self.entries {
            if entry.state == State::Planned
                && let Err(error) = entry
                    .scan
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .verify(&entry.scan.path)
            {
                entry.state = State::Failed(error.to_string());
            }
        }
        loop {
            self.propagate_blocked(&locations, true);
            let pending = self
                .entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| (e.state == State::Planned).then_some(i))
                .collect::<Vec<_>>();
            if pending.is_empty() {
                break;
            }
            if let Some(&i) = pending
                .iter()
                .find(|&&i| matches!(self.dependency(i, &locations), Ok(None)))
            {
                let target = self.entries[i].target.as_ref().unwrap();
                let result = snapshots[i]
                    .as_ref()
                    .unwrap()
                    .verify(&locations[i])
                    .and_then(|()| move_file(&locations[i], target));
                self.entries[i].state = match result {
                    Ok(()) => {
                        locations[i] = target.clone();
                        State::Applied
                    }
                    Err(error) => State::Failed(error.to_string()),
                };
                continue;
            }
            // All remaining destinations depend on moving occupants: break one cycle.
            // An entry is staged at most once, bounding progress even if other processes interfere.
            let Some(&i) = pending.iter().find(|&&i| !staged[i]) else {
                for i in pending {
                    self.entries[i].state =
                        State::Failed("rename dependencies changed during apply".into());
                }
                break;
            };
            let result = (|| -> io::Result<()> {
                let before = snapshots[i].as_ref().unwrap();
                before.verify(&locations[i])?;
                let temporary = temporary_name(&locations[i])?;
                move_file(&locations[i], &temporary)?;
                locations[i] = temporary;
                staged[i] = true;
                let after = Snapshot::take(&locations[i])?;
                // Our rename may change ctime; identity, length and content mtime must not change.
                if before.identity != after.identity
                    || before.len != after.len
                    || before.modified != after.modified
                {
                    return Err(io::Error::other("source changed while staging rename"));
                }
                snapshots[i] = Some(after);
                Ok(())
            })();
            if let Err(error) = result {
                self.entries[i].state = State::Failed(error.to_string());
            }
        }
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if staged[i]
                && let State::Failed(reason) = &mut entry.state
            {
                reason.push_str(&format!(
                    "; temporary file left as {} in the original folder",
                    name(locations[i].file_name().unwrap())
                ));
            }
        }
    }
    pub(crate) fn summary(&self, apply: bool) -> (String, u8) {
        let (mut identified, mut unknown, mut ambiguous, mut errors) = (0, 0, 0, 0);
        let (mut planned, mut correct, mut conflicts, mut applied, mut failed) = (0, 0, 0, 0, 0);
        let mut out = String::new();
        for e in &self.entries {
            match &e.scan.outcome {
                Ok(MatchOutcome::Identified { .. }) => identified += 1,
                Ok(MatchOutcome::Unknown { .. }) => unknown += 1,
                Ok(MatchOutcome::Ambiguous { .. }) => ambiguous += 1,
                Err(_) => errors += 1,
            }
            match &e.state {
                State::Planned => planned += 1,
                State::AlreadyCorrect => correct += 1,
                State::Conflict(_) => conflicts += 1,
                State::Applied => {
                    planned += 1;
                    applied += 1;
                    out.push_str(&format!(
                        "Renamed: {} → {}\n",
                        name(e.scan.path.file_name().unwrap()),
                        name(e.target.as_ref().unwrap().file_name().unwrap())
                    ));
                }
                State::Failed(reason) => {
                    planned += 1;
                    failed += 1;
                    out.push_str(&format!(
                        "Rename failed: {} — {}\n",
                        name(e.scan.path.file_name().unwrap()),
                        text(reason)
                    ));
                }
                State::Untouched => (),
            }
        }
        out.push_str(&format!("Results: {identified} identified, {unknown} unknown, {ambiguous} ambiguous, {errors} errors.\nRenames: {planned} planned, {correct} already correct, {conflicts} conflicted, {applied} applied, {failed} failed.\n"));
        if correct == self.entries.len() && correct > 0 {
            out.push_str("No changes needed.\n");
        } else if apply && failed > 0 {
            out.push_str(
                "Apply incomplete; successful renames were kept. No rollback attempted.\n",
            );
        }
        let code = if errors + conflicts + failed > 0 {
            1
        } else if ambiguous > 0 {
            3
        } else if unknown > 0 {
            2
        } else {
            0
        };
        (out, code)
    }
}
pub(crate) fn confirm(input: &mut impl io::Read) -> io::Result<bool> {
    let mut line = Vec::with_capacity(64);
    for _ in 0..65 {
        let mut byte = [0];
        if input.read(&mut byte)? == 0 {
            return Ok(false);
        }
        if byte[0] == b'\n' {
            return Ok(std::str::from_utf8(&line)
                .is_ok_and(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "y" | "yes")));
        }
        if line.len() == 64 {
            return Ok(false);
        }
        line.push(byte[0]);
    }
    Ok(false)
}
fn destination_occupant(path: &Path) -> io::Result<Option<PathBuf>> {
    check_folder(path.parent().unwrap())?;
    let exists = match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    let key = collision_key(path.file_name().unwrap());
    let mut occupant = None;
    for (count, entry) in fs::read_dir(path.parent().unwrap())?.enumerate() {
        if count >= 256 {
            return Err(io::Error::other("folder enumeration exceeds 256 entries"));
        }
        let entry = entry?;
        if collision_key(&entry.file_name()) == key {
            if occupant.is_some() {
                return Err(io::Error::other(
                    "multiple case-colliding destination occupants",
                ));
            }
            occupant = Some(entry.path());
        }
    }
    if exists && occupant.is_none() {
        return Err(io::Error::other(
            "destination exists through an unresolved filesystem alias",
        ));
    }
    Ok(occupant)
}
fn temporary_name(source: &Path) -> io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let ext = source
        .extension()
        .and_then(|s| s.to_str())
        .filter(|s| {
            ["mkv", "mp4", "m4v"]
                .iter()
                .any(|ext| s.eq_ignore_ascii_case(ext))
        })
        .ok_or_else(|| io::Error::other("unsupported temporary extension"))?;
    for _ in 0..16 {
        let path = source.with_file_name(format!(
            ".tvmatch-rename-{}-{nonce}-{}.{ext}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        if destination_occupant(&path)?.is_none() {
            return Ok(path);
        }
    }
    Err(io::Error::other(
        "could not choose an unused temporary rename name",
    ))
}
#[cfg(test)]
mod tests;
