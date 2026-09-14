use media_mkv_webm::open_streaming;
use std::fs::File;
use std::time::Instant;

// Opt-in local input only; no private media paths are bundled.
fn main() {
    let paths: Vec<_> = std::env::args_os().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: mkv_smoke PATH [PATH ...]");
        std::process::exit(1);
    }
    for (index, path) in paths.iter().enumerate() {
        let start = Instant::now();
        let file = File::open(path).expect("open");
        let mut s = open_streaming(file).expect("open_streaming");
        let elapsed = start.elapsed();
        println!(
            "input[{index}]: parsed in {elapsed:?}, tracks={}, cues={}, clusters_off={}",
            s.demuxer.tracks.len(),
            s.demuxer.cues.len(),
            s.demuxer.clusters_offset
        );
        for t in &s.demuxer.tracks {
            println!(
                "  track {} kind={:?} codec={}",
                t.number, t.kind, t.codec_id
            );
        }
        // Walk first 5 frames
        for i in 0..5 {
            match s.next_frame().expect("next_frame") {
                Some(f) => println!(
                    "  frame[{i}] track={} ts={} key={:?} len={}",
                    f.track,
                    f.timestamp_ns,
                    f.is_keyframe,
                    f.data.len()
                ),
                None => {
                    println!("  frame[{i}] = None");
                    break;
                }
            }
        }
    }
}
