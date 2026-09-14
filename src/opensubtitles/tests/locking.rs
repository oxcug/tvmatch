use super::*;
use std::{sync::mpsc, thread, time::Duration};
struct PausedDownload {
    mock: Mock,
    entered: mpsc::Sender<()>,
    resume: mpsc::Receiver<()>,
}
impl Transport for PausedDownload {
    fn api(&mut self, path: &str, body: Option<Value>) -> Result<Value> {
        if body.is_some() {
            self.entered.send(()).unwrap();
            self.resume
                .recv_timeout(Duration::from_secs(15))
                .map_err(|_| fail("synthetic download pause timed out"))?;
        }
        self.mock.api(path, body)
    }
    fn content(&mut self, url: &str) -> Result<Vec<u8>> {
        self.mock.content(url)
    }
    fn authenticated(&self) -> bool {
        self.mock.authenticated()
    }
    fn quota(&mut self) -> Result<Option<u64>> {
        self.mock.quota()
    }
}
#[test]
fn paused_download_does_not_hold_shared_cache_transaction_or_allow_same_season_replay() {
    let root = temp();
    let worker_root = root.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let cache = Cache::open(&worker_root).unwrap();
        let mut m = manifest();
        m.scope = scope();
        m.selected.truncate(1);
        cache.freeze(&m).unwrap();
        let mut mock = Mock::new(Some(10));
        mock.responses.push_back(Ok(
            json!({"remaining":9,"link":"https://www.opensubtitles.com/synthetic"}),
        ));
        let mut paused = PausedDownload {
            mock,
            entered: entered_tx,
            resume: resume_rx,
        };
        acquire(&cache, &m, &mut paused).unwrap();
        assert_eq!((paused.mock.posts, paused.mock.contents), (1, 1));
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let cache = Cache::open(&root).unwrap();
    let mut same = manifest();
    same.scope = scope();
    same.selected.truncate(1);
    let mut blocked = Mock::new(Some(10));
    assert!(acquire(&cache, &same, &mut blocked).is_err());
    assert_eq!((blocked.posts, blocked.contents), (0, 0));
    let mut other = same;
    other.scope.season = 2;
    other.selected[0].season = 2;
    other.selected[0].episode_id += 100;
    cache.freeze(&other).unwrap();
    let mut online = Mock::new(Some(10));
    online.responses.push_back(Ok(
        json!({"remaining":9,"link":"https://www.opensubtitles.com/synthetic"}),
    ));
    acquire(&cache, &other, &mut online).unwrap();
    assert_eq!((online.posts, online.contents), (1, 1));
    assert_eq!(cache.references(&other).unwrap().len(), 1);
    resume_tx.send(()).unwrap();
    worker.join().unwrap();
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
