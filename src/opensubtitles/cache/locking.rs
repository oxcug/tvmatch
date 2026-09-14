//! Process-owned OS locks. Persistent paths are never unlinked: deleting a locked
//! inode would let another process lock a different inode under the same name.
use super::{Result, check_ancestors, ensure_dir, exists, fail, new_file, read};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs::{File, OpenOptions, TryLockError},
    io::Write,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    time::{Duration, Instant},
};
pub(super) type Season = (u64, u32);
const FENCE: &[u8] = b"tvmatch scoped-cache-locks-v2; do not delete\n";
const MAX_LOCK_FILES: usize = 20_000;
pub(super) struct Guard {
    _file: File,
}
fn open_file(path: &Path) -> Result<File> {
    check_ancestors(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| fail("cache coordination file open failed"))?;
    if !file
        .metadata()
        .map_err(|_| fail("cache coordination metadata failed"))?
        .is_file()
    {
        return Err(fail("cache coordination file is not regular"));
    }
    Ok(file)
}
fn try_guard(path: &Path) -> Result<Option<Guard>> {
    let file = open_file(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Guard { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(_) => Err(fail(
            "OS cache locking unavailable; refusing unlocked access",
        )),
    }
}
pub(super) struct Locks {
    root: PathBuf,
    transaction: RefCell<Weak<Guard>>,
    seasons: RefCell<BTreeMap<Season, Guard>>,
}
impl Locks {
    pub(super) fn open(root: &Path) -> Result<Self> {
        let locks = Self {
            root: root.into(),
            transaction: Default::default(),
            seasons: Default::default(),
        };
        let _guard = locks.transaction()?;
        // Fence legacy create-new root-lock clients. Never remove an old live/stale
        // lock to migrate: only an absent path can be initialized automatically.
        let fence = root.join(".lock");
        if !exists(&fence)? {
            new_file(&fence, FENCE)?;
        }
        if read(&fence, 128)? != FENCE {
            return Err(fail(
                "legacy cache root lock present; let the older tvmatch process exit normally; do not remove a live lock",
            ));
        }
        ensure_dir(&root.join(".locks-v2"))?;
        Ok(locks)
    }
    pub(super) fn transaction(&self) -> Result<Rc<Guard>> {
        if let Some(guard) = self.transaction.borrow().upgrade() {
            return Ok(guard);
        }
        let start = Instant::now();
        loop {
            if let Some(guard) = try_guard(&self.root.join(".coordination-v2.lock"))? {
                let guard = Rc::new(guard);
                *self.transaction.borrow_mut() = Rc::downgrade(&guard);
                return Ok(guard);
            }
            if start.elapsed() >= Duration::from_secs(30) {
                return Err(fail(
                    "cache housekeeping busy for 30 seconds; retry after other cache I/O finishes",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn path(&self, key: Season) -> PathBuf {
        self.root
            .join(".locks-v2")
            .join(format!("show-{}-season-{}.lock", key.0, key.1))
    }
    pub(super) fn claim(&self, key: Season) -> Result<()> {
        let _guard = self.transaction()?;
        if key.0 == 0 || key.0 > i32::MAX as u64 || key.1 == 0 || key.1 > 100 {
            return Err(fail("invalid cache lock season identity"));
        }
        if self.owns(key) {
            return Ok(());
        }
        let Some(guard) = try_guard(&self.path(key))? else {
            return Err(fail(&format!(
                "reference cache busy for provider show {} season {}; another tvmatch run owns this season",
                key.0, key.1
            )));
        };
        self.write_budget(key, 0)?;
        self.seasons.borrow_mut().insert(key, guard);
        Ok(())
    }
    pub(super) fn owns(&self, key: Season) -> bool {
        self.seasons.borrow().contains_key(&key)
    }
    pub(super) fn budget(&self, key: Season) -> Result<u64> {
        let bytes = read(&self.path(key).with_extension("budget"), 8)?;
        Ok(u64::from_be_bytes(
            bytes
                .try_into()
                .map_err(|_| fail("invalid active cache reservation"))?,
        ))
    }
    fn write_budget(&self, key: Season, bytes: u64) -> Result<()> {
        // Separate from the locked file: Windows byte-range locks also prevent
        // other handles reading its contents. Shared transaction protects this IO.
        let path = self.path(key).with_extension("budget");
        let mut file = open_file(&path)?;
        file.set_len(0)
            .and_then(|_| file.write_all(&bytes.to_be_bytes()))
            .and_then(|_| file.sync_all())
            .map_err(|_| fail("cache reservation write failed"))
    }
    pub(super) fn reserve(&self, key: Season, bytes: u64) -> Result<()> {
        let _guard = self.transaction()?;
        if !self.owns(key) {
            return Err(fail("cache reservation requires season ownership"));
        }
        self.write_budget(key, bytes)
    }
    pub(super) fn consume(&self, key: Season, bytes: u64) -> Result<()> {
        let _guard = self.transaction()?;
        self.reserve(key, self.budget(key)?.saturating_sub(bytes))
    }
    pub(super) fn active(&self) -> Result<BTreeMap<Season, u64>> {
        let _guard = self.transaction()?;
        let dir = self.root.join(".locks-v2");
        check_ancestors(&dir)?;
        let mut active = BTreeMap::new();
        for (count, entry) in std::fs::read_dir(dir)
            .map_err(|_| fail("cache lock inventory failed"))?
            .enumerate()
        {
            if count >= MAX_LOCK_FILES {
                return Err(fail("cache lock inventory limit"));
            }
            let entry = entry.map_err(|_| fail("cache lock inventory failed"))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some((show, season)) = name
                .strip_prefix("show-")
                .and_then(|n| n.strip_suffix(".lock"))
                .and_then(|n| n.split_once("-season-"))
            else {
                continue;
            };
            let (Ok(show), Ok(season)) = (show.parse::<u64>(), season.parse::<u32>()) else {
                return Err(fail("invalid cache lock name"));
            };
            let key = (show, season);
            if entry.path() != self.path(key)
                || show == 0
                || show > i32::MAX as u64
                || season == 0
                || season > 100
            {
                return Err(fail("invalid cache lock identity"));
            }
            if self.owns(key) || try_guard(&entry.path())?.is_none() {
                active.insert(key, self.budget(key)?);
            }
            // Inactive/crashed owners' budget files are ignored, never replayed.
        }
        Ok(active)
    }
}
