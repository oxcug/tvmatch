//! Read-only bounded diagnostic. Writes private crops ONLY to a caller-supplied
//! existing directory; create_new refuses overwrites. PPM is black-composited;
//! PAM retains straight RGBA. Not OCR or whole-file validation when stopped.
#[cfg(feature = "media")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use media_mkv_webm::streaming::StreamingLimits;
    use std::{
        fs::{File, OpenOptions},
        io::Write,
        ops::ControlFlow,
        path::PathBuf,
    };
    use tvmatch::media::pgs::{PgsLimits, scan_pgs};
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err(
            "usage: pgs_images INPUT.mkv EXISTING_TEMP_DIRECTORY MAX_VISIBLE_IMAGES".into(),
        );
    }
    let directory = PathBuf::from(&args[2]);
    if !directory.is_dir() {
        return Err("output directory must already exist".into());
    }
    let max: usize = args[3].parse()?;
    if !(1..=20).contains(&max) {
        return Err("image count must be 1..20".into());
    }
    let mut count = 0;
    let mut output_error = None;
    let started = std::time::Instant::now();
    let result = scan_pgs(
        File::open(&args[1])?,
        8,
        StreamingLimits {
            skip_cues: true,
            ..Default::default()
        },
        PgsLimits::default(),
        |d| {
            let Some(image) = &d.image else {
                return ControlFlow::Continue(());
            };
            if d.unchanged {
                return ControlFlow::Continue(());
            }
            count += 1;
            let base = format!("subtitle-{count:02}-{}ns", d.timestamp_ns);
            let write = || -> std::io::Result<()> {
                let pam_path = directory.join(format!("{base}.pam"));
                let ppm_path = directory.join(format!("{base}.ppm"));
                let mut pam = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&pam_path)?;
                write!(
                    pam,
                    "P7\nWIDTH {}\nHEIGHT {}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n",
                    image.rect.width, image.rect.height
                )?;
                pam.write_all(&image.rgba)?;
                let mut ppm = std::io::BufWriter::new(
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&ppm_path)?,
                );
                write!(ppm, "P6\n{} {}\n255\n", image.rect.width, image.rect.height)?;
                for p in image.rgba.chunks_exact(4) {
                    ppm.write_all(&[
                        ((u16::from(p[0]) * u16::from(p[3]) + 127) / 255) as u8,
                        ((u16::from(p[1]) * u16::from(p[3]) + 127) / 255) as u8,
                        ((u16::from(p[2]) * u16::from(p[3]) + 127) / 255) as u8,
                    ])?;
                }
                ppm.flush()?;
                println!(
                    "UNVERIFIED time_ns={} canvas={}x{} rect={:?} composition={} state=0x{:02x} objects={} PAM={} PPM={}",
                    d.timestamp_ns,
                    d.canvas_width,
                    d.canvas_height,
                    image.rect,
                    d.composition_number,
                    d.composition_state,
                    d.objects.len(),
                    pam_path.display(),
                    ppm_path.display()
                );
                Ok(())
            };
            if let Err(e) = write() {
                output_error = Some(e);
                return ControlFlow::Break(());
            }
            if count >= max {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
    );
    if let Some(error) = output_error {
        return Err(error.into());
    }
    println!("summary={:?} elapsed={:?}", result?, started.elapsed());
    Ok(())
}
#[cfg(not(feature = "media"))]
fn main() {
    eprintln!("rebuild with --features media");
    std::process::exit(1);
}
