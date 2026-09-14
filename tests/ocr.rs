#![cfg(feature = "ocr")]
//! No copyrighted fixtures, models, downloads or external OCR in ordinary tests.
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "tvmatch-ocr-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[cfg(feature = "ocr")]
#[test]
fn local_model_invalid_and_oversized_files_fail_without_inference() {
    use tvmatch::media::ocr::{LocalOcr, OcrError};
    let s = Scratch::new();
    let model = s.0.join("invalid.rten");
    fs::write(&model, b"original invalid model bytes").unwrap();
    assert!(matches!(
        LocalOcr::load(None, &model),
        Err(OcrError::Model(_))
    ));
    fs::File::options()
        .write(true)
        .open(&model)
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    assert!(matches!(
        LocalOcr::load(None, &model),
        Err(OcrError::BudgetExceeded("model bytes"))
    ));
}

#[test]
fn bundled_model_size_digest_and_usable_load() {
    let bytes = include_bytes!("../assets/ocr/text-recognition.rten");
    assert_eq!(bytes.len(), 9_716_444);
    #[cfg(feature = "opensubtitles")]
    {
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!("{:x}", Sha256::digest(bytes)),
            "606d9a0414c6b73c99df75b707c11c70d1c8b12e1d4f900922e185fc37bfca65"
        );
    }
    tvmatch::media::ocr::LocalOcr::bundled().unwrap();
}
