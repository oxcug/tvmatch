//! Optional CPU OCR of PGS crops. Text is fallible derived evidence, not a
//! container declaration or episode identity. Models must be trusted local assets.
use super::{
    MediaError,
    pgs::{self, PgsDisplay, PgsImage, PgsLimits, PgsScanSummary},
};
use crate::srt::{Cue, MAX_CUE_BYTES, MAX_CUES, MAX_SRT_BYTES, Transcript};
use media_mkv_webm::streaming::StreamingLimits;
use ocrs::{ImageSource, OcrEngine, OcrEngineParams};
use std::{
    error::Error,
    fmt,
    fs::File,
    io::{Read, Seek},
    ops::ControlFlow,
    path::Path,
};

const BUNDLED_RECOGNITION: &[u8] = include_bytes!("../../assets/ocr/text-recognition.rten");

const MAX_MODEL_BYTES: usize = 16 * 1024 * 1024;
const MARGIN: usize = 16;
const MAX_SOURCE_WIDTH: usize = 3840;
const MAX_SOURCE_HEIGHT: usize = 2160;
const MAX_CROP_HEIGHT: usize = 512;
const ROW_GAP: usize = 12;
const MAX_IMAGE_PIXELS: usize = 2 * 1024 * 1024;
const MAX_TOTAL_PIXELS: usize = 128 * 1024 * 1024;
const MAX_WORDS: usize = 256;
const MAX_LINES: usize = 16;

#[derive(Debug)]
pub enum OcrError {
    Io(std::io::Error),
    Model(String),
    Inference(String),
    Media(MediaError),
    BudgetExceeded(&'static str),
    InvalidImage,
    InvalidFrameLimit,
    UnreadableLayout,
    NoText,
    Transcript(crate::srt::ParseError),
}
impl fmt::Display for OcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OCR: {self:?}")
    }
}
impl Error for OcrError {}

/// One engine reused serially, no frame queues or raster cache. RTen uses its
/// CPU-count-bounded pool (RTEN_NUM_THREADS can reduce it); no subprocess/network.
/// File caps do NOT sandbox a malicious model graph or bound inference RSS.
/// Supply trusted compatible weights, not models embedded in untrusted media.
pub struct LocalOcr {
    engine: OcrEngine,
    use_detector: bool,
}
impl LocalOcr {
    /// Embedded, unmodified CC-BY-SA-4.0 recognizer; no runtime model file or detector.
    pub fn bundled() -> Result<Self, OcrError> {
        let recognition_model = rten::Model::load(BUNDLED_RECOGNITION.to_vec())
            .map_err(|e| OcrError::Model(e.to_string()))?;
        let engine = OcrEngine::new(OcrEngineParams {
            recognition_model: Some(recognition_model),
            ..Default::default()
        })
        .map_err(|e| OcrError::Model(e.to_string()))?;
        Ok(Self {
            engine,
            use_detector: false,
        })
    }

    /// No detector selects deterministic horizontal subtitle line layout;
    /// supplying a detector selects its geometry, with no automatic fallback.
    pub fn load(detection: Option<&Path>, recognition: &Path) -> Result<Self, OcrError> {
        let detection_model = detection.map(load_model).transpose()?;
        let recognition_model = load_model(recognition)?;
        let engine = OcrEngine::new(OcrEngineParams {
            detection_model,
            recognition_model: Some(recognition_model),
            ..Default::default()
        })
        .map_err(|e| OcrError::Model(e.to_string()))?;
        Ok(Self {
            engine,
            use_detector: detection.is_some(),
        })
    }
    fn recognize(&self, grey: &[u8], width: u32, height: u32) -> Result<String, OcrError> {
        rten::thread_pool().run(|| {
            let err = |e: String| OcrError::Inference(e);
            let source =
                ImageSource::from_bytes(grey, (width, height)).map_err(|e| err(e.to_string()))?;
            let input = self
                .engine
                .prepare_input(source)
                .map_err(|e| err(e.to_string()))?;
            let lines = if self.use_detector {
                let words = self
                    .engine
                    .detect_words(&input)
                    .map_err(|e| err(e.to_string()))?;
                if words.len() > MAX_WORDS {
                    return Err(OcrError::BudgetExceeded("detected words"));
                }
                self.engine.find_text_lines(&input, &words)
            } else {
                subtitle_lines(grey, width, height)?
                    .into_iter()
                    .map(|r| vec![rten_imageproc::RotatedRect::from_rect(r.to_f32())])
                    .collect()
            };
            if lines.len() > MAX_LINES {
                return Err(OcrError::BudgetExceeded("detected lines"));
            }
            if lines.iter().any(Vec::is_empty) {
                return Err(OcrError::Inference("empty detected line".into()));
            }
            let text = self
                .engine
                .recognize_text(&input, &lines)
                .map_err(|e| err(e.to_string()))?;
            let mut out = String::new();
            for line in text.into_iter().flatten() {
                let line = line.to_string();
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if out.len() + line.len() + 1 > MAX_CUE_BYTES {
                    return Err(OcrError::BudgetExceeded("cue text"));
                }
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(line);
            }
            Ok(out)
        })
    }
}
fn load_model(path: &Path) -> Result<rten::Model, OcrError> {
    let file = File::open(path).map_err(OcrError::Io)?;
    if !file.metadata().map_err(OcrError::Io)?.is_file() {
        return Err(OcrError::Model("model must be a regular local file".into()));
    }
    let mut bytes = Vec::new();
    file.take((MAX_MODEL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(OcrError::Io)?;
    if bytes.len() > MAX_MODEL_BYTES {
        return Err(OcrError::BudgetExceeded("model bytes"));
    }
    rten::Model::load(bytes).map_err(|e| OcrError::Model(e.to_string()))
}

fn source_dimensions(image: &PgsImage) -> Result<(usize, usize), OcrError> {
    let w = usize::from(image.rect.width);
    let h = usize::from(image.rect.height);
    if w > MAX_SOURCE_WIDTH || h > MAX_SOURCE_HEIGHT {
        return Err(OcrError::BudgetExceeded("source crop dimensions"));
    }
    if w == 0 || h == 0 || image.rgba.len() != w * h * 4 {
        return Err(OcrError::InvalidImage);
    }
    Ok((w, h))
}
fn pixel_grey(p: &[u8]) -> u8 {
    let luminance =
        (77 * u32::from(p[0]) + 150 * u32::from(p[1]) + 29 * u32::from(p[2]) + 128) / 256;
    255 - ((luminance * u32::from(p[3]) + 127) / 255) as u8
}
/// Resolve alpha onto black and invert, with a fixed 16px white margin. Ordinary
/// <=512px crops are unchanged. Taller canvas-spanning crops may remove only fully
/// white vertical whitespace, keeping >12 rows between bands. No glyph is scaled,
/// thresholded, dropped or reordered; model input and line limits stay unchanged.
fn prepare_crop(image: &PgsImage) -> Result<(Vec<u8>, u32, u32), OcrError> {
    let (w, h) = source_dimensions(image)?;
    let mut bands = Vec::new();
    if h <= MAX_CROP_HEIGHT {
        bands.push((0, h));
    } else {
        for y in 0..h {
            if image.rgba[y * w * 4..(y + 1) * w * 4]
                .chunks_exact(4)
                .all(|p| pixel_grey(p) == 255)
            {
                continue;
            }
            if let Some((_, end)) = bands.last_mut().filter(|(_, end)| y - *end <= ROW_GAP) {
                *end = y + 1;
            } else {
                if bands.len() == MAX_LINES {
                    return Err(OcrError::BudgetExceeded("subtitle lines"));
                }
                bands.push((y, y + 1));
            }
        }
        if bands.is_empty() {
            bands.push((0, 1));
        } // Empty raster remains empty, not invented text.
    }
    let packed_height = bands.iter().map(|(a, b)| b - a).sum::<usize>()
        + bands.len().saturating_sub(1) * (ROW_GAP + 1);
    if packed_height > MAX_CROP_HEIGHT {
        return Err(OcrError::BudgetExceeded("compacted crop dimensions"));
    }
    let width = w + 2 * MARGIN;
    let height = packed_height + 2 * MARGIN;
    if width * height > MAX_IMAGE_PIXELS {
        return Err(OcrError::BudgetExceeded("crop pixels"));
    }
    let mut grey = vec![255; width * height];
    let mut output_y = MARGIN;
    for (start, end) in bands {
        for y in start..end {
            for x in 0..w {
                grey[output_y * width + x + MARGIN] =
                    pixel_grey(&image.rgba[(y * w + x) * 4..][..4]);
            }
            output_y += 1;
        }
        output_y += ROW_GAP + 1;
    }
    Ok((grey, width as u32, height as u32))
}

#[derive(Debug, Clone)]
pub struct OcrTimestamp {
    pub start_ns: u64,
    /// Next effective replacement/clear, not BlockDuration. Unknown at EOF/stop.
    pub end_ns: Option<u64>,
    /// SRT requires positive duration; absent/equal/sub-ms ends use start_ms+1.
    pub transcript_end_synthetic: bool,
}
#[derive(Debug)]
pub struct PgsOcr {
    pub transcript: Transcript,
    pub timestamps: Vec<OcrTimestamp>,
    pub scan: PgsScanSummary,
    pub frames_recognized: usize,
    pub empty_frames: usize,
    pub unchanged_displays: usize,
    pub input_pixels: usize,
}
struct Collector {
    cues: Vec<Cue>,
    times: Vec<OcrTimestamp>,
    active: Option<usize>,
    frames: usize,
    empty: usize,
    unchanged: usize,
    pixels: usize,
    text_bytes: usize,
}
impl Collector {
    fn new() -> Self {
        Self {
            cues: vec![],
            times: vec![],
            active: None,
            frames: 0,
            empty: 0,
            unchanged: 0,
            pixels: 0,
            text_bytes: 0,
        }
    }
    fn push(
        &mut self,
        display: &PgsDisplay,
        recognize: &mut impl FnMut(&[u8], u32, u32) -> Result<String, OcrError>,
    ) -> Result<(), OcrError> {
        if display.unchanged {
            self.unchanged += 1;
            return Ok(());
        }
        if let Some(i) = self.active.take() {
            let time = &mut self.times[i];
            time.end_ns = Some(display.timestamp_ns);
            let end = display.timestamp_ns / 1_000_000;
            time.transcript_end_synthetic = end <= self.cues[i].start_ms;
            if !time.transcript_end_synthetic {
                self.cues[i].end_ms = end;
            }
        }
        let Some(image) = &display.image else {
            return Ok(());
        };
        if self.frames == MAX_CUES {
            return Err(OcrError::BudgetExceeded("OCR frames"));
        }
        // Charge the FULL source raster before whitespace compaction/inference.
        // Validate source bounds first, so padded arithmetic is safe on 32-bit too.
        let (w, h) = source_dimensions(image)?;
        let pixels = (w + 2 * MARGIN) * (h + 2 * MARGIN);
        if pixels > MAX_TOTAL_PIXELS - self.pixels {
            return Err(OcrError::BudgetExceeded("cumulative OCR pixels"));
        }
        let (grey, width, height) = prepare_crop(image)?;
        self.pixels += pixels;
        self.frames += 1;
        let text = recognize(&grey, width, height)?;
        if text.trim().is_empty() {
            self.empty += 1;
            return Ok(());
        }
        if text.len() > MAX_CUE_BYTES || text.len() > MAX_SRT_BYTES - self.text_bytes {
            return Err(OcrError::BudgetExceeded("OCR text bytes"));
        }
        self.text_bytes += text.len();
        self.active = Some(self.cues.len());
        let start_ms = display.timestamp_ns / 1_000_000;
        self.cues.push(Cue {
            start_ms,
            end_ms: start_ms + 1,
            text,
        });
        self.times.push(OcrTimestamp {
            start_ns: display.timestamp_ns,
            end_ns: None,
            transcript_end_synthetic: true,
        });
        Ok(())
    }
}

/// None requires clean EOF; Some(1..=4096) explicitly samples that many distinct
/// visible rasters, returning Stopped (unread suffix unvalidated). Blank OCR still
/// counts as work. Any decoder/model/text error discards all collected evidence.
/// Bounds are independent of unchanged finite container/PGS defaults.
pub fn extract_pgs_ocr<R: Read + Seek>(
    reader: R,
    track: u64,
    engine: &LocalOcr,
    max_frames: Option<usize>,
) -> Result<PgsOcr, OcrError> {
    extract_pgs_ocr_with_limits(
        reader,
        track,
        engine,
        max_frames,
        StreamingLimits::default(),
    )
}

/// Caller-selected finite container budgets, cumulative across open and walk.
/// This forward-only OCR path always skips Cues. PGS/OCR allocation and work
/// caps are unchanged; exhaustion returns an error, never a sampled success.
/// `extract_pgs_ocr` retains the canonical default budgets.
pub fn extract_pgs_ocr_with_limits<R: Read + Seek>(
    reader: R,
    track: u64,
    engine: &LocalOcr,
    max_frames: Option<usize>,
    container_limits: StreamingLimits,
) -> Result<PgsOcr, OcrError> {
    extract_with_limits(reader, track, max_frames, container_limits, |grey, w, h| {
        engine.recognize(grey, w, h)
    })
}
/// Reuse one forward scan, decoder and OCR collector across increasing frame checkpoints.
/// The final checkpoint is a hard sample cap. Blank OCR counts toward each checkpoint.
/// Callback cues are provisional until this function succeeds: a stop still validates
/// the rest of its containing packet. Any later error discards the extraction.
/// Continue must reassess the complete accumulated cues, not add earlier match scores.
/// Clean EOF before a checkpoint returns the accumulated transcript for final assessment.
pub fn extract_pgs_ocr_progressive_with_limits<R: Read + Seek>(
    reader: R,
    track: u64,
    engine: &LocalOcr,
    checkpoints: &[usize],
    container_limits: StreamingLimits,
    assess: impl FnMut(usize, &[Cue]) -> ControlFlow<()>,
) -> Result<PgsOcr, OcrError> {
    if checkpoints.is_empty() {
        return Err(OcrError::InvalidFrameLimit);
    }
    extract_checkpoints_with(
        reader,
        track,
        checkpoints,
        container_limits,
        |grey, w, h| engine.recognize(grey, w, h),
        assess,
    )
}
#[cfg(test)]
fn extract_with<R: Read + Seek>(
    reader: R,
    track: u64,
    max_frames: Option<usize>,
    recognize: impl FnMut(&[u8], u32, u32) -> Result<String, OcrError>,
) -> Result<PgsOcr, OcrError> {
    extract_with_limits(
        reader,
        track,
        max_frames,
        StreamingLimits::default(),
        recognize,
    )
}
fn extract_with_limits<R: Read + Seek>(
    reader: R,
    track: u64,
    max_frames: Option<usize>,
    container_limits: StreamingLimits,
    recognize: impl FnMut(&[u8], u32, u32) -> Result<String, OcrError>,
) -> Result<PgsOcr, OcrError> {
    let checkpoints = max_frames.into_iter().collect::<Vec<_>>();
    extract_checkpoints_with(
        reader,
        track,
        &checkpoints,
        container_limits,
        recognize,
        |_, _| ControlFlow::Break(()),
    )
}
fn extract_checkpoints_with<R: Read + Seek>(
    reader: R,
    track: u64,
    checkpoints: &[usize],
    container_limits: StreamingLimits,
    mut recognize: impl FnMut(&[u8], u32, u32) -> Result<String, OcrError>,
    mut assess: impl FnMut(usize, &[Cue]) -> ControlFlow<()>,
) -> Result<PgsOcr, OcrError> {
    if checkpoints.len() > MAX_CUES
        || checkpoints.iter().any(|&n| n == 0 || n > MAX_CUES)
        || checkpoints.windows(2).any(|w| w[0] >= w[1])
    {
        return Err(OcrError::InvalidFrameLimit);
    }
    let mut next_checkpoint = 0;
    let mut collector = Collector::new();
    let mut failure = None;
    let scan = pgs::scan_pgs(
        reader,
        track,
        StreamingLimits {
            skip_cues: true,
            ..container_limits
        },
        PgsLimits::default(),
        |display| {
            if let Err(e) = collector.push(display, &mut recognize) {
                failure = Some(e);
                return ControlFlow::Break(());
            }
            if checkpoints
                .get(next_checkpoint)
                .is_some_and(|&n| collector.frames >= n)
            {
                next_checkpoint += 1;
                if assess(collector.frames, &collector.cues).is_break()
                    || next_checkpoint == checkpoints.len()
                {
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        },
    )
    .map_err(OcrError::Media)?;
    if let Some(e) = failure {
        return Err(e);
    }
    if collector.cues.is_empty() {
        return Err(OcrError::NoText);
    }
    let transcript = Transcript::from_cues(collector.cues).map_err(OcrError::Transcript)?;
    Ok(PgsOcr {
        transcript,
        timestamps: collector.times,
        scan,
        frames_recognized: collector.frames,
        empty_frames: collector.empty,
        unchanged_displays: collector.unchanged,
        input_pixels: collector.pixels,
    })
}

/// Fixed row-gap policy for isolated horizontal subtitles, not general page
/// layout. Keep every non-white pixel (including accents/punctuation), merging
/// gaps up to 12 rows. Vertically overlapping speakers form one line; columns,
/// rotated text and gaps >12 inside glyphs are outside this layout assumption.
fn subtitle_lines(
    grey: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<rten_imageproc::Rect>, OcrError> {
    let w = width as usize;
    let h = height as usize;
    if w == 0
        || h == 0
        || w > MAX_SOURCE_WIDTH + 2 * MARGIN
        || h > MAX_CROP_HEIGHT + 2 * MARGIN
        || w * h > MAX_IMAGE_PIXELS
        || w * h != grey.len()
    {
        return Err(OcrError::InvalidImage);
    }
    let mut bands: Vec<(usize, usize)> = Vec::new();
    for y in 0..h {
        if grey[y * w..(y + 1) * w].iter().all(|&p| p == 255) {
            continue;
        }
        if let Some((_, end)) = bands.last_mut().filter(|(_, end)| y - *end <= ROW_GAP) {
            *end = y + 1;
        } else {
            if bands.len() == MAX_LINES {
                return Err(OcrError::BudgetExceeded("subtitle lines"));
            }
            bands.push((y, y + 1));
        }
    }
    if bands.is_empty() {
        return Err(OcrError::UnreadableLayout);
    }
    let mut rects = Vec::new();
    for (top, bottom) in bands {
        let mut left = w;
        let mut right = 0;
        for y in top..bottom {
            for x in 0..w {
                if grey[y * w + x] != 255 {
                    left = left.min(x);
                    right = right.max(x + 1);
                }
            }
        }
        // Four-pixel recognition margin, clipped to the checked input. Adjacent
        // bands have >12px gap, so these expanded rectangles cannot overlap.
        let top = top.saturating_sub(4);
        let bottom = (bottom + 4).min(h);
        let left = left.saturating_sub(4);
        let right = (right + 4).min(w);
        if top >= bottom || left >= right {
            return Err(OcrError::UnreadableLayout);
        }
        rects.push(rten_imageproc::Rect::from_tlhw(
            top as i32,
            left as i32,
            (bottom - top) as i32,
            (right - left) as i32,
        ));
    }
    Ok(rects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgs::Rect;
    #[test]
    #[ignore = "opt-in read-only local PGS geometry diagnostic; no media fixture required by CI"]
    fn inspect_local_pgs_crop_geometry() {
        let Some(path) = std::env::var_os("TVMATCH_TEST_PGS_FILE") else {
            return;
        };
        let track: u64 = std::env::var("TVMATCH_TEST_PGS_TRACK")
            .unwrap()
            .parse()
            .unwrap();
        let file = File::open(&path).unwrap();
        let before = file.metadata().unwrap();
        let mut frames = 0;
        let mut found = false;
        let limits = StreamingLimits {
            elements: 1_000_000,
            io_operations: 4_000_000,
            ..StreamingLimits::default()
        };
        let summary = pgs::scan_pgs(file, track, limits, PgsLimits::default(), |display| {
            if display.unchanged { return ControlFlow::Continue(()); }
            let Some(image) = &display.image else { return ControlFlow::Continue(()); };
            frames += 1;
            let w = usize::from(image.rect.width);
            let h = usize::from(image.rect.height);
            if h > 512 || w > 3840 {
                let mut bands: Vec<(usize,usize)> = Vec::new();
                for y in 0..h {
                    let ink = image.rgba[y*w*4..(y+1)*w*4].chunks_exact(4).any(|p| {
                        let l=(77*u32::from(p[0])+150*u32::from(p[1])+29*u32::from(p[2])+128)/256;
                        (l*u32::from(p[3])+127)/255 > 0
                    });
                    if !ink {continue;}
                    if let Some((_,end))=bands.last_mut().filter(|(_,end)|y-*end<=12) {*end=y+1;} else {bands.push((y,y+1));}
                }
                let packed_height: usize = bands.iter().map(|(a,b)|b-a).sum::<usize>() + bands.len().saturating_sub(1)*13;
                println!("frame={frames} timestamp_ns={} source={}x{} canvas={}x{} bands={} first_bands={:?} packed_height={packed_height}",display.timestamp_ns,w,h,display.canvas_width,display.canvas_height,bands.len(),&bands[..bands.len().min(8)]);
                found = true;
                return ControlFlow::Break(());
            }
            if frames == 192 {ControlFlow::Break(())} else {ControlFlow::Continue(())}
        }).unwrap();
        println!(
            "frames={frames} completion={:?} oversized_found={found}",
            summary.completion
        );
        let after = std::fs::metadata(path).unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    }
    fn image(w: u16, h: u16, rgba: Vec<u8>) -> PgsImage {
        PgsImage {
            rect: Rect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
            rgba,
        }
    }
    fn display(ns: u64, visible: bool, unchanged: bool) -> PgsDisplay {
        PgsDisplay {
            timestamp_ns: ns,
            previous_timestamp_ns: None,
            canvas_width: 100,
            canvas_height: 100,
            frame_rate_code: 0,
            composition_number: 0,
            composition_state: 0,
            palette_id: 0,
            palette_update: false,
            objects: vec![],
            image: visible.then(|| image(1, 1, vec![255; 4])),
            unchanged,
        }
    }
    fn ink(grey: &mut [u8], w: usize, x: usize, y: usize, width: usize, height: usize) {
        for row in y..y + height {
            for col in x..x + width {
                grey[row * w + col] = 0;
            }
        }
    }
    #[test]
    fn prepare_crop_composites_alpha_inverts_and_preserves_antialiasing() {
        let (grey, w, h) = prepare_crop(&image(
            4,
            1,
            vec![
                255, 255, 255, 0, 255, 255, 255, 128, 0, 0, 0, 255, 255, 255, 255, 255,
            ],
        ))
        .unwrap();
        assert_eq!((w, h), (36, 33));
        assert_eq!(&grey[16 * 36 + 16..16 * 36 + 20], &[255, 127, 255, 0]);
        assert!(grey[..16 * 36].iter().all(|&p| p == 255));
    }
    #[test]
    fn prepare_crop_rejects_invalid_and_oversized_images_before_allocation() {
        for img in [
            image(0, 1, vec![]),
            image(2, 1, vec![0; 4]),
            image(65535, 65535, vec![]),
            image(3841, 1, vec![]),
            image(1, 513, vec![]),
        ] {
            assert!(prepare_crop(&img).is_err());
        }
        assert!(matches!(
            prepare_crop(&image(3840, 512, vec![255; 3840 * 512 * 4])),
            Err(OcrError::BudgetExceeded("crop pixels"))
        ));
    }
    #[test]
    fn subtitle_lines_one_two_ordered_and_clipped() {
        let mut grey = vec![255; 100 * 100];
        ink(&mut grey, 100, 20, 10, 50, 20);
        let one = subtitle_lines(&grey, 100, 100).unwrap();
        assert_eq!(one, vec![rten_imageproc::Rect::from_tlhw(6, 16, 28, 58)]);
        ink(&mut grey, 100, 0, 60, 100, 40);
        let two = subtitle_lines(&grey, 100, 100).unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(two[1], rten_imageproc::Rect::from_tlhw(56, 0, 44, 100));
        assert_eq!(two, subtitle_lines(&grey, 100, 100).unwrap());
    }
    #[test]
    fn subtitle_lines_keeps_detached_accents_punctuation_and_overlapping_columns() {
        let mut grey = vec![255; 100 * 100];
        ink(&mut grey, 100, 20, 10, 1, 1); // Detached accent, not noise.
        ink(&mut grey, 100, 20, 22, 25, 15); // 11-row gap joins accent.
        ink(&mut grey, 100, 90, 36, 1, 1); // Punctuation at far right.
        ink(&mut grey, 100, 60, 25, 10, 5); // Overlapping column explicitly one band.
        let lines = subtitle_lines(&grey, 100, 100).unwrap();
        assert_eq!(lines, vec![rten_imageproc::Rect::from_tlhw(6, 16, 35, 79)]);
        ink(&mut grey, 100, 5, 60, 1, 1); // Isolated punctuation line retained.
        assert_eq!(subtitle_lines(&grey, 100, 100).unwrap().len(), 2);
    }
    #[test]
    fn subtitle_lines_empty_padding_malformed_and_count_limits() {
        assert!(matches!(
            subtitle_lines(&[255; 100], 10, 10),
            Err(OcrError::UnreadableLayout)
        ));
        for (w, h) in [(0, 0), (u32::MAX, u32::MAX), (100, 100)] {
            assert!(subtitle_lines(&[], w, h).is_err());
        }
        let (grey, w, h) = prepare_crop(&image(1, 1, vec![255, 255, 255, 0])).unwrap();
        assert!(matches!(
            subtitle_lines(&grey, w, h),
            Err(OcrError::UnreadableLayout)
        ));
        let mut grey = vec![255; 10 * 300];
        for y in (0..17 * 15).step_by(15) {
            ink(&mut grey, 10, 5, y, 1, 1);
        }
        assert!(matches!(
            subtitle_lines(&grey, 10, 300),
            Err(OcrError::BudgetExceeded("subtitle lines"))
        ));
    }
    #[test]
    fn tall_sparse_crops_preserve_every_composited_pixel_and_band_order() {
        // Measured failure geometry: 833x916, top sign + two bottom lines.
        let w = 833usize;
        let h = 916usize;
        let bands = [(4, 57), (803, 855), (871, 912)];
        let mut rgba = vec![0; w * h * 4];
        for (i, (start, end)) in bands.iter().enumerate() {
            for y in *start..*end {
                for x in 3 + i * 20..23 + i * 20 {
                    rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[
                        220,
                        170,
                        110,
                        127 + (i as u8) * 30,
                    ]);
                }
            }
        }
        let source = image(w as u16, h as u16, rgba);
        let (grey, width, height) = prepare_crop(&source).unwrap();
        assert_eq!((width, height), (865, 204)); // 172 packed rows plus margins.
        let mut expected = vec![255; grey.len()];
        let mut dest = MARGIN;
        for (start, end) in bands {
            for y in start..end {
                for x in 0..w {
                    expected[dest * width as usize + x + MARGIN] =
                        pixel_grey(&source.rgba[(y * w + x) * 4..][..4]);
                }
                dest += 1;
            }
            dest += ROW_GAP + 1;
        }
        assert_eq!(grey, expected);
        let lines = subtitle_lines(&grey, width, height).unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines.windows(2).all(|p| p[0].bottom() < p[1].top()));
    }
    #[test]
    fn tall_compaction_preserves_detached_glyph_gaps_and_separate_lines() {
        let mut rgba = vec![0; 916 * 4];
        for y in [5, 18, 800] {
            rgba[y * 4..y * 4 + 4].fill(255);
        }
        let (grey, w, h) = prepare_crop(&image(1, 916, rgba)).unwrap();
        assert_eq!(h, 60);
        let ink_rows = (0..h as usize)
            .filter(|y| {
                grey[y * w as usize..(y + 1) * w as usize]
                    .iter()
                    .any(|p| *p != 255)
            })
            .collect::<Vec<_>>();
        assert_eq!(ink_rows, [16, 29, 43]); // Original 12-row glyph gap; 13-row inter-line gap.
        assert_eq!(subtitle_lines(&grey, w, h).unwrap().len(), 2);
    }
    #[test]
    fn ordinary_crop_geometry_and_pixels_remain_byte_identical() {
        let w = 37usize;
        let h = 80usize;
        let rgba = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let source = image(w as u16, h as u16, rgba);
        let (grey, width, height) = prepare_crop(&source).unwrap();
        assert_eq!((width, height), ((w + 32) as u32, (h + 32) as u32));
        let mut expected = vec![255; (w + 32) * (h + 32)];
        for y in 0..h {
            for x in 0..w {
                expected[(y + 16) * (w + 32) + x + 16] =
                    pixel_grey(&source.rgba[(y * w + x) * 4..][..4]);
            }
        }
        assert_eq!(grey, expected);
    }
    #[test]
    fn whitespace_compaction_does_not_relax_source_dense_content_or_line_limits() {
        for source in [
            image(3841, 1, vec![]),
            image(1, 2161, vec![]),
            image(u16::MAX, u16::MAX, vec![]),
            image(1, 900, vec![0; 4]),
        ] {
            assert!(prepare_crop(&source).is_err());
        }
        assert!(matches!(
            prepare_crop(&image(1, 513, vec![255; 513 * 4])),
            Err(OcrError::BudgetExceeded("compacted crop dimensions"))
        ));
        let mut rgba = vec![0; 900 * 4];
        for y in (0..17 * 20).step_by(20) {
            rgba[y * 4..y * 4 + 4].fill(255);
        }
        assert!(matches!(
            prepare_crop(&image(1, 900, rgba)),
            Err(OcrError::BudgetExceeded("subtitle lines"))
        ));
        let (grey, _, h) = prepare_crop(&image(1, 2160, vec![0; 2160 * 4])).unwrap();
        assert_eq!(h, 33);
        assert!(grey.iter().all(|p| *p == 255));
    }
    #[test]
    fn tall_crop_counts_one_frame_and_charges_full_source_before_inference() {
        let mut d = display(1_000_001, true, false);
        let mut rgba = vec![0; 900 * 4];
        rgba[4..8].fill(255);
        rgba[899 * 4..900 * 4].fill(255);
        d.image = Some(image(1, 900, rgba));
        let source_pixels = (1 + 32) * (900 + 32);
        let mut c = Collector::new();
        c.pixels = MAX_TOTAL_PIXELS - source_pixels + 1;
        assert!(matches!(
            c.push(&d, &mut |_, _, _| panic!("must reject before inference")),
            Err(OcrError::BudgetExceeded("cumulative OCR pixels"))
        ));
        let mut c = Collector::new();
        let mut calls = 0;
        c.push(&d, &mut |_, _, h| {
            calls += 1;
            assert!(h < 544);
            Ok("First line.\nSecond line.".into())
        })
        .unwrap();
        assert_eq!((calls, c.frames, c.pixels), (1, 1, source_pixels));
        assert_eq!(c.cues.len(), 1);
        assert_eq!(c.times[0].start_ns, 1_000_001);
        c.push(&display(8_000_001, false, false), &mut |_, _, _| panic!())
            .unwrap();
        assert_eq!(c.times[0].end_ns, Some(8_000_001));
    }
    #[test]
    fn collector_preserves_ns_replacement_clear_unchanged_and_unknown_ends() {
        let mut c = Collector::new();
        let mut calls = 0;
        let mut read = |_: &[u8], _: u32, _: u32| {
            calls += 1;
            Ok("Original synthetic caption.".into())
        };
        c.push(&display(1_000_001, true, false), &mut read).unwrap();
        c.push(&display(2_000_999, true, true), &mut read).unwrap();
        c.push(&display(3_000_999, false, false), &mut read)
            .unwrap();
        c.push(&display(4_000_001, true, false), &mut read).unwrap();
        c.push(&display(4_000_002, true, false), &mut read).unwrap();
        assert_eq!(calls, 3);
        assert_eq!(c.unchanged, 1);
        assert_eq!(
            (c.times[0].start_ns, c.times[0].end_ns),
            (1_000_001, Some(3_000_999))
        );
        assert!(!c.times[0].transcript_end_synthetic);
        assert_eq!(c.cues[0].end_ms, 3);
        assert!(c.times[1].transcript_end_synthetic);
        assert_eq!(c.times[1].end_ns, Some(4_000_002));
        assert_eq!(c.times[2].end_ns, None);
    }
    #[test]
    fn collector_blank_text_is_not_evidence_or_a_reused_caption() {
        let mut c = Collector::new();
        c.push(&display(0, true, false), &mut |_, _, _| {
            Ok("Original caption".into())
        })
        .unwrap();
        c.push(&display(5_000_000, true, false), &mut |_, _, _| {
            Ok(String::new())
        })
        .unwrap();
        c.push(&display(8_000_000, false, false), &mut |_, _, _| panic!())
            .unwrap();
        assert_eq!(c.empty, 1);
        assert_eq!(c.frames, 2);
        assert_eq!(c.cues.len(), 1);
        assert_eq!(c.times[0].end_ns, Some(5_000_000));
    }
    #[test]
    fn collector_work_and_text_budgets_precede_inference_or_append() {
        let d = display(0, true, false);
        let mut c = Collector::new();
        c.frames = MAX_CUES;
        assert!(matches!(
            c.push(&d, &mut |_, _, _| panic!()),
            Err(OcrError::BudgetExceeded("OCR frames"))
        ));
        let mut c = Collector::new();
        c.pixels = MAX_TOTAL_PIXELS;
        assert!(matches!(
            c.push(&d, &mut |_, _, _| panic!()),
            Err(OcrError::BudgetExceeded("cumulative OCR pixels"))
        ));
        let mut c = Collector::new();
        c.text_bytes = MAX_SRT_BYTES;
        assert!(matches!(
            c.push(&d, &mut |_, _, _| Ok("x".into())),
            Err(OcrError::BudgetExceeded("OCR text bytes"))
        ));
        let mut c = Collector::new();
        assert!(matches!(
            c.push(&d, &mut |_, _, _| Ok("x".repeat(MAX_CUE_BYTES + 1))),
            Err(OcrError::BudgetExceeded("OCR text bytes"))
        ));
    }
    #[test]
    fn extraction_rejects_bad_frame_limits_before_media_or_models() {
        for limit in [0, MAX_CUES + 1] {
            assert!(matches!(
                extract_with(std::io::Cursor::new([]), 1, Some(limit), |_, _, _| panic!()),
                Err(OcrError::InvalidFrameLimit)
            ));
        }
    }
    #[test]
    fn bundled_recognizer_runs_on_original_synthetic_glyphs() {
        let engine = LocalOcr::bundled().unwrap();
        let mut grey = vec![255; 180 * 60];
        let glyphs = [
            [17u8, 17, 17, 31, 17, 17, 17], // H
            [31, 16, 16, 30, 16, 16, 31],   // E
            [16, 16, 16, 16, 16, 16, 31],   // L
            [16, 16, 16, 16, 16, 16, 31],   // L
            [14, 17, 17, 17, 17, 17, 14],   // O
        ];
        for (letter, rows) in glyphs.iter().enumerate() {
            for (y, row) in rows.iter().enumerate() {
                for x in 0..5 {
                    if row & (1 << (4 - x)) != 0 {
                        ink(&mut grey, 180, 16 + letter * 24 + x * 4, 16 + y * 4, 4, 4);
                    }
                }
            }
        }
        let text = engine.recognize(&grey, 180, 60).unwrap();
        assert!(text.len() <= MAX_CUE_BYTES); // Inference smoke, not font/OCR accuracy calibration.
    }
    #[test]
    fn missing_model_returns_io_not_text() {
        assert!(matches!(
            LocalOcr::load(None, Path::new("not-a-supplied-tvmatch-model.rten")),
            Err(OcrError::Io(_))
        ));
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use media_mkv_webm::{DocType, Muxer, TrackDescriptor, TrackKind};
    use std::io::Cursor;
    fn seg(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut b = vec![kind];
        b.extend((body.len() as u16).to_be_bytes());
        b.extend(body);
        b
    }
    fn packet() -> Vec<u8> {
        [
            seg(
                0x16,
                &[
                    0, 16, 0, 8, 0x10, 0, 0, 0x80, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0,
                ],
            ),
            seg(0x17, &[1, 0, 0, 0, 0, 0, 0, 16, 0, 8]),
            seg(0x14, &[0, 0, 1, 235, 128, 128, 255]),
            seg(0x15, &[0, 1, 0, 0xc0, 0, 0, 7, 0, 1, 0, 1, 1, 0, 0]),
            seg(0x80, &[]),
        ]
        .concat()
    }
    fn container(packets: &[Vec<u8>]) -> Vec<u8> {
        let mut mux = Muxer::new(DocType::Matroska);
        mux.register_track(TrackDescriptor {
            kind: TrackKind::Subtitle,
            codec_id: "S_HDMV/PGS".into(),
            ..Default::default()
        })
        .unwrap();
        for (i, p) in packets.iter().enumerate() {
            mux.append(1, p, i as u64 * 10_000_000, true).unwrap();
        }
        mux.finalize().unwrap()
    }
    fn varied_packet(i: usize) -> Vec<u8> {
        let mut bytes = packet();
        let mut offset = 0;
        while offset < bytes.len() {
            let length = u16::from_be_bytes([bytes[offset + 1], bytes[offset + 2]]) as usize;
            if bytes[offset] == 0x14 {
                bytes[offset + 6] = 32 + (i % 25) as u8 * 7;
                return bytes;
            }
            offset += 3 + length;
        }
        panic!("fixture palette missing");
    }
    #[test]
    fn progressive_checkpoints_share_one_collector_and_stop_without_prefix_replay() {
        let bytes = container(&(0..7).map(varied_packet).collect::<Vec<_>>());
        let mut calls = 0;
        let mut checkpoints = Vec::new();
        let result = extract_checkpoints_with(
            Cursor::new(&bytes),
            1,
            &[2, 4, 6],
            StreamingLimits::default(),
            |_, _, _| {
                calls += 1;
                Ok(format!("synthetic caption {calls}"))
            },
            |frames, cues| {
                checkpoints.push((frames, cues.len()));
                ControlFlow::Continue(())
            },
        )
        .unwrap();
        assert_eq!(calls, 6);
        assert_eq!(checkpoints, [(2, 2), (4, 4), (6, 6)]);
        assert_eq!(result.frames_recognized, 6);
        assert_eq!(result.scan.completion, pgs::ScanCompletion::Stopped);
        let mut calls = 0;
        let result = extract_checkpoints_with(
            Cursor::new(&bytes),
            1,
            &[2, 4, 6],
            StreamingLimits::default(),
            |_, _, _| {
                calls += 1;
                Ok("synthetic caption".into())
            },
            |_, _| ControlFlow::Break(()),
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(result.frames_recognized, 2);
        for invalid in [
            vec![0],
            vec![2, 2],
            vec![3, 2],
            vec![MAX_CUES + 1],
            vec![1; MAX_CUES + 1],
        ] {
            assert!(matches!(
                extract_checkpoints_with(
                    Cursor::new([]),
                    1,
                    &invalid,
                    StreamingLimits::default(),
                    |_, _, _| panic!(),
                    |_, _| panic!()
                ),
                Err(OcrError::InvalidFrameLimit)
            ));
        }
    }
    #[test]
    fn progressive_blank_frames_count_and_eof_keeps_later_cues() {
        let bytes = container(&(0..5).map(varied_packet).collect::<Vec<_>>());
        let mut calls = 0;
        let mut seen = Vec::new();
        let result = extract_checkpoints_with(
            Cursor::new(&bytes),
            1,
            &[2, 6],
            StreamingLimits::default(),
            |_, _, _| {
                calls += 1;
                Ok(if calls <= 2 {
                    String::new()
                } else {
                    "later caption".into()
                })
            },
            |frames, cues| {
                seen.push((frames, cues.len()));
                ControlFlow::Continue(())
            },
        )
        .unwrap();
        assert_eq!(seen, [(2, 0)]);
        assert_eq!(calls, 5);
        assert_eq!(result.empty_frames, 2);
        assert_eq!(result.transcript.cues().len(), 3);
        assert_eq!(result.scan.completion, pgs::ScanCompletion::Complete);
    }
    #[test]
    fn progressive_callbacks_are_provisional_until_packet_and_later_validation() {
        let mut corrupt = packet();
        corrupt.extend_from_slice(&[0x80, 0, 1]);
        let mut callbacks = 0;
        assert!(matches!(
            extract_checkpoints_with(
                Cursor::new(container(&[corrupt])),
                1,
                &[1, 2],
                StreamingLimits::default(),
                |_, _, _| Ok("synthetic".into()),
                |_, _| {
                    callbacks += 1;
                    ControlFlow::Break(())
                }
            ),
            Err(OcrError::Media(_))
        ));
        assert_eq!(callbacks, 1);
        assert!(matches!(
            extract_checkpoints_with(
                Cursor::new(container(&[packet(), vec![0x80, 0, 1]])),
                1,
                &[1, 2],
                StreamingLimits::default(),
                |_, _, _| Ok("synthetic".into()),
                |_, _| ControlFlow::Continue(())
            ),
            Err(OcrError::Media(_))
        ));
        let mut calls = 0;
        assert!(matches!(
            extract_checkpoints_with(
                Cursor::new(container(&[varied_packet(0), varied_packet(1)])),
                1,
                &[1, 2],
                StreamingLimits::default(),
                |_, _, _| {
                    calls += 1;
                    if calls == 2 {
                        Err(OcrError::Inference("later failure".into()))
                    } else {
                        Ok("synthetic".into())
                    }
                },
                |_, _| ControlFlow::Continue(())
            ),
            Err(OcrError::Inference(_))
        ));
    }
    #[test]
    fn progressive_continues_inside_one_packet_and_repetition_can_remove_support() {
        let combined = (0..7).flat_map(varied_packet).collect::<Vec<_>>();
        let mut calls = 0;
        let mut seen = Vec::new();
        let result = extract_checkpoints_with(
            Cursor::new(container(&[combined])),
            1,
            &[2, 4, 6],
            StreamingLimits::default(),
            |_, _, _| {
                calls += 1;
                Ok("synthetic".into())
            },
            |frames, _| {
                seen.push(frames);
                ControlFlow::Continue(())
            },
        )
        .unwrap();
        assert_eq!(calls, 6);
        assert_eq!(seen, [2, 4, 6]);
        assert_eq!(result.scan.packets, 1);
        let captions = (0..3)
            .map(|i| format!("alpha{i} beta{i} gamma{i} delta{i}"))
            .collect::<Vec<_>>();
        let transcript = Transcript::from_cues(
            captions
                .iter()
                .enumerate()
                .map(|(i, text)| Cue {
                    start_ms: i as u64 * 10_000,
                    end_ms: i as u64 * 10_000 + 1,
                    text: text.clone(),
                })
                .collect(),
        )
        .unwrap();
        let index = crate::Index::build(vec![
            crate::Reference::new(
                crate::ReferenceId::new("synthetic", "one").unwrap(),
                "one",
                "synthetic",
                transcript,
            )
            .unwrap(),
        ])
        .unwrap();
        let mut mux = Muxer::new(DocType::Matroska);
        mux.register_track(TrackDescriptor {
            kind: TrackKind::Subtitle,
            codec_id: "S_HDMV/PGS".into(),
            ..Default::default()
        })
        .unwrap();
        for i in 0..6 {
            mux.append(1, &varied_packet(i as usize), i * 10_000_000_000, true)
                .unwrap();
        }
        let mut calls = 0;
        let mut scores = Vec::new();
        extract_checkpoints_with(
            Cursor::new(mux.finalize().unwrap()),
            1,
            &[2, 6],
            StreamingLimits::default(),
            |_, _, _| {
                let text = captions[calls % 3].clone();
                calls += 1;
                Ok(text)
            },
            |_, cues| {
                let outcome = index
                    .match_query(&Transcript::from_cues(cues.to_vec()).unwrap())
                    .unwrap();
                let crate::MatchOutcome::Unknown { candidates, .. } = outcome else {
                    panic!("repetition must not manufacture identification")
                };
                scores.push(candidates.first().map_or(0, |c| c.score));
                ControlFlow::Continue(())
            },
        )
        .unwrap();
        assert_eq!(scores, [2, 0]);
    }
    fn episode_limits() -> StreamingLimits {
        StreamingLimits {
            elements: 1_000_000,
            io_operations: 4_000_000,
            ..Default::default()
        }
    }
    #[test]
    fn extract_with_limits_default_exhaustion_and_configured_full_walk() {
        let mut mux = Muxer::new(DocType::Matroska);
        for (kind, codec) in [
            (TrackKind::Subtitle, "S_HDMV/PGS"),
            (TrackKind::Video, "V_TEST"),
        ] {
            mux.register_track(TrackDescriptor {
                kind,
                codec_id: codec.into(),
                ..Default::default()
            })
            .unwrap();
        }
        mux.append(1, &packet(), 0, true).unwrap();
        // Many unselected blocks still cost traversal work, not OCR frames.
        for _ in 0..100_001 {
            mux.append(2, &[42], 1_000_000, true).unwrap();
        }
        mux.append(1, &packet(), 2_000_000, true).unwrap();
        let bytes = mux.finalize().unwrap();
        let read = |_: &[u8], _: u32, _: u32| Ok("Original synthetic caption.".into());
        let error = extract_with(Cursor::new(&bytes), 1, None, read).unwrap_err();
        assert!(
            error.to_string().contains("strict element budget exceeded"),
            "{error}"
        );
        let full =
            extract_with_limits(Cursor::new(&bytes), 1, None, episode_limits(), read).unwrap();
        assert_eq!(full.scan.completion, pgs::ScanCompletion::Complete);
        assert_eq!(full.scan.packets, 2);
        assert_eq!(full.scan.displays, 2);
        assert_eq!(
            full.transcript.cues()[0].text,
            "Original synthetic caption."
        );
        let mut assessed = 0;
        let error = extract_checkpoints_with(
            Cursor::new(&bytes),
            1,
            &[1, 2],
            StreamingLimits::default(),
            read,
            |_, _| {
                assessed += 1;
                ControlFlow::Continue(())
            },
        )
        .unwrap_err();
        assert_eq!(assessed, 1);
        assert!(error.to_string().contains("strict element budget exceeded"));
        assert_eq!(StreamingLimits::default().elements, 100_000);
        assert_eq!(StreamingLimits::default().io_operations, 1_000_000);
        assert_eq!(StreamingLimits::default().read_bytes, 64 * 1024 * 1024);
        // Larger traversal limits do not disable independent byte limits.
        assert!(
            extract_with_limits(
                Cursor::new(&bytes),
                1,
                None,
                StreamingLimits {
                    read_bytes: 1024,
                    ..episode_limits()
                },
                read
            )
            .unwrap_err()
            .to_string()
            .contains("read byte budget")
        );
    }
    #[test]
    fn extract_with_limits_late_corruption_truncation_and_io_fail_closed() {
        use std::io::{Read, SeekFrom};
        let read = |_: &[u8], _: u32, _: u32| Ok("Original synthetic caption.".into());
        let bad = container(&[packet(), vec![0x80, 0, 1]]);
        assert!(matches!(
            extract_with_limits(Cursor::new(&bad), 1, None, episode_limits(), read),
            Err(OcrError::Media(_))
        ));
        let good = container(&[packet()]);
        assert!(matches!(
            extract_with_limits(
                Cursor::new(&good[..good.len() - 1]),
                1,
                None,
                episode_limits(),
                read
            ),
            Err(OcrError::Media(_))
        ));
        struct Fault {
            data: Cursor<Vec<u8>>,
            fail: std::rc::Rc<std::cell::Cell<bool>>,
        }
        impl Read for Fault {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.fail.get() {
                    return Err(std::io::Error::other("injected full walk read"));
                }
                self.data.read(buf)
            }
        }
        impl Seek for Fault {
            fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
                if self.fail.get() {
                    return Err(std::io::Error::other("injected full walk seek"));
                }
                self.data.seek(pos)
            }
        }
        let fail = std::rc::Rc::new(std::cell::Cell::new(false));
        let error = extract_with_limits(
            Fault {
                data: Cursor::new(good),
                fail: fail.clone(),
            },
            1,
            None,
            episode_limits(),
            |_, _, _| {
                fail.set(true);
                Ok("Original synthetic caption.".into())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("injected full walk"), "{error}");
    }
    #[test]
    fn extract_with_complete_stopped_and_late_errors_never_fake_completion() {
        let good = container(&[packet()]);
        let read = |_: &[u8], _: u32, _: u32| Ok("An original synthetic sentence.".into());
        let full = extract_with(Cursor::new(&good), 1, None, read).unwrap();
        assert_eq!(full.scan.completion, pgs::ScanCompletion::Complete);
        assert_eq!(full.timestamps[0].end_ns, None);
        assert!(full.timestamps[0].transcript_end_synthetic);
        let stopped = extract_with(Cursor::new(&good), 1, Some(1), read).unwrap();
        assert_eq!(stopped.scan.completion, pgs::ScanCompletion::Stopped);
        let bad = container(&[packet(), vec![0x80, 0, 1]]);
        assert!(matches!(
            extract_with(Cursor::new(&bad), 1, None, read),
            Err(OcrError::Media(_))
        ));
        assert_eq!(
            extract_with(Cursor::new(&bad), 1, Some(1), read)
                .unwrap()
                .scan
                .completion,
            pgs::ScanCompletion::Stopped
        );
        assert!(matches!(
            extract_with(Cursor::new(&good), 1, None, |_, _, _| Err(
                OcrError::Inference("synthetic failure".into())
            )),
            Err(OcrError::Inference(_))
        ));
        assert!(matches!(
            extract_with(Cursor::new(&good), 1, None, |_, _, _| Ok(String::new())),
            Err(OcrError::NoText)
        ));
        assert!(matches!(
            extract_with(Cursor::new(&good), 1, None, |_, _, _| Ok(
                "bad\u{000c}text".into()
            )),
            Err(OcrError::Transcript(_))
        ));
    }
}
