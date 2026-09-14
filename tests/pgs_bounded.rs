#![cfg(feature = "media")]
// Original tiny rasters, not private subtitle/video extracts or tool-generated fixtures.
mod support;
// Run the crate's acquisition safety oracles against the actual dependency too.
#[path = "../crates/media-mkv-webm/src/pgs/tests.rs"]
mod acquisition_regressions;
use media_mkv_webm::{ebml::schema::ids, streaming::StreamingLimits};
use std::{io::Cursor, ops::ControlFlow};
use support::*;
use tvmatch::media::{MediaError, pgs::*};

fn seg(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut b = vec![kind];
    b.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
    b.extend_from_slice(body);
    b
}
fn join(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}
fn rect(x: u16, y: u16, w: u16, h: u16) -> Vec<u8> {
    [x, y, w, h]
        .into_iter()
        .flat_map(u16::to_be_bytes)
        .collect()
}
fn reference(id: u16, window: u8, flags: u8, x: u16, y: u16, crop: Option<[u16; 4]>) -> Vec<u8> {
    let mut b = id.to_be_bytes().to_vec();
    b.extend([window, flags]);
    b.extend(x.to_be_bytes());
    b.extend(y.to_be_bytes());
    if let Some([x, y, w, h]) = crop {
        b.extend(rect(x, y, w, h));
    }
    b
}
fn pcs(state: u8, update: bool, palette: u8, refs: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![
        0,
        16,
        0,
        8,
        0x10,
        0,
        7,
        state,
        if update { 0x80 } else { 0 },
        palette,
        refs.len() as u8,
    ];
    b.extend(refs.concat());
    seg(0x16, &b)
}
fn wds(windows: &[(u8, [u16; 4])]) -> Vec<u8> {
    let mut b = vec![windows.len() as u8];
    for (id, [x, y, w, h]) in windows {
        b.push(*id);
        b.extend(rect(*x, *y, *w, *h));
    }
    seg(0x17, &b)
}
fn pds(id: u8, entries: &[[u8; 5]]) -> Vec<u8> {
    let mut b = vec![id, 0];
    b.extend(entries.iter().flatten());
    seg(0x14, &b)
}
fn ods(id: u16, version: u8, flags: u8, dims: Option<(u16, u16, usize)>, data: &[u8]) -> Vec<u8> {
    let mut b = id.to_be_bytes().to_vec();
    b.extend([version, flags]);
    if let Some((w, h, len)) = dims {
        let len = (len + 4) as u32;
        b.extend(&len.to_be_bytes()[1..]);
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
    }
    b.extend(data);
    seg(0x15, &b)
}
fn end() -> Vec<u8> {
    seg(0x80, &[])
}
fn basic_with_rle(w: u16, h: u16, rle: &[u8]) -> Vec<u8> {
    join(&[
        pcs(0x80, false, 0, &[reference(1, 0, 0x40, 2, 1, None)]),
        wds(&[(0, [0, 0, 16, 8])]),
        pds(
            0,
            &[
                [0, 16, 128, 128, 0],
                [1, 235, 128, 128, 255],
                [2, 16, 128, 128, 128],
            ],
        ),
        ods(1, 0, 0xc0, Some((w, h, rle.len())), rle),
        end(),
    ])
}
fn basic() -> Vec<u8> {
    basic_with_rle(2, 1, &[1, 1, 0, 0])
}
fn decode(packet: &[u8]) -> Result<Vec<u8>, PgsError> {
    let mut decoder = PgsDecoder::new(Default::default());
    let mut image = Vec::new();
    decoder.push_packet(123, packet, |d| {
        if let Some(i) = &d.image {
            image = i.rgba.clone();
        }
    })?;
    decoder.finish()?;
    Ok(image)
}
fn fails(packet: &[u8]) {
    assert!(decode(packet).is_err());
}

#[test]
fn pgs_acquisition_stop_request_still_rejects_malformed_same_packet_tail() {
    let mut packet = basic();
    packet[10] = 0x40;
    packet.push(0x14);
    let bytes = muxed(
        &[("S_HDMV/PGS", media_mkv_webm::TrackKind::Subtitle)],
        &[(1, 10, &packet)],
    );
    let mut callbacks = 0;
    let result = scan_pgs(
        Cursor::new(bytes),
        1,
        StreamingLimits::default(),
        PgsLimits::default(),
        |_| {
            callbacks += 1;
            ControlFlow::Break(())
        },
    );
    assert!(matches!(result, Err(MediaError::Pgs(_))));
    assert_eq!(callbacks, 1);
}
#[test]
fn pgs_acquisition_mkv_stream_preserves_images_and_interval_metadata() {
    let mut acquisition = basic();
    acquisition[10] = 0x40; // PCS composition state; all resources are supplied afresh.
    assert_eq!(decode(&acquisition).unwrap(), decode(&basic()).unwrap());
    let normal = join(&[
        pcs(0, false, 0, &[reference(1, 0, 0x40, 2, 1, None)]),
        end(),
    ]);
    let bytes = muxed(
        &[("S_HDMV/PGS", media_mkv_webm::TrackKind::Subtitle)],
        &[(1, 10, &basic()), (1, 20, &acquisition), (1, 30, &normal)],
    );
    let mut displays = Vec::new();
    let summary = scan_pgs(
        Cursor::new(bytes),
        1,
        StreamingLimits::default(),
        PgsLimits::default(),
        |d| {
            displays.push((
                d.timestamp_ns,
                d.previous_timestamp_ns,
                d.composition_state,
                d.image.as_ref().unwrap().rgba.clone(),
            ));
            ControlFlow::Continue(())
        },
    )
    .unwrap();
    assert_eq!(summary.completion, ScanCompletion::Complete);
    assert_eq!(displays.len(), 3);
    assert_eq!(
        (displays[1].0, displays[1].1, displays[1].2),
        (20_000_000, Some(10_000_000), 0x40)
    );
    assert_eq!(displays[0].3, displays[1].3);
    assert_eq!(displays[1].3, displays[2].3);
}
#[test]
fn pgs_basic_rgba_and_source_metadata() {
    let mut decoder = PgsDecoder::new(Default::default());
    decoder
        .push_packet(123_456_789, &basic(), |d| {
            assert_eq!(d.timestamp_ns, 123_456_789);
            assert_eq!(d.previous_timestamp_ns, None);
            assert_eq!((d.canvas_width, d.canvas_height), (16, 8));
            assert_eq!(d.composition_number, 7);
            assert_eq!(d.frame_rate_code, 0x10);
            assert!(d.objects[0].forced);
            assert!(!d.unchanged);
            let i = d.image.as_ref().unwrap();
            assert_eq!(
                i.rect,
                Rect {
                    x: 2,
                    y: 1,
                    width: 2,
                    height: 1
                }
            );
            assert_eq!(i.rgba, vec![255; 8]);
        })
        .unwrap();
    decoder.finish().unwrap();
}
#[test]
fn pgs_all_rle_run_forms_and_explicit_eol() {
    // Literal, short zero, short color, long zero, long color; two exact rows.
    let rle = [
        1, 0, 2, 0, 0x82, 1, 0, 0x40, 1, 0, 0xc0, 2, 2, 0, 0, 0, 0x88, 1, 0, 0,
    ];
    let image = decode(&basic_with_rle(8, 2, &rle)).unwrap();
    assert_eq!(image.len(), 64);
    assert_eq!(&image[0..4], &[255, 255, 255, 255]);
    assert_eq!(&image[4..12], &[0; 8]);
    assert_eq!(&image[24..32], &[0, 0, 0, 128, 0, 0, 0, 128]);
    assert_eq!(&image[32..], &[255; 32]);
}
#[test]
fn pgs_fragmented_object_and_display_set_across_packets() {
    let mut d = PgsDecoder::new(Default::default());
    let mut calls = 0;
    let pre = join(&[
        pcs(0x80, false, 0, &[reference(1, 0, 0, 0, 0, None)]),
        wds(&[(0, [0, 0, 16, 8])]),
        pds(0, &[[1, 235, 128, 128, 255]]),
        ods(1, 3, 0x80, Some((2, 1, 4)), &[1]),
    ]);
    d.push_packet(100, &pre, |_| calls += 1).unwrap();
    assert_eq!(calls, 0);
    d.push_packet(101, &ods(1, 3, 0, None, &[1]), |_| calls += 1)
        .unwrap();
    d.push_packet(
        102,
        &join(&[ods(1, 3, 0x40, None, &[0, 0]), end()]),
        |image| {
            calls += 1;
            assert_eq!(image.timestamp_ns, 100);
            assert_eq!(image.image.as_ref().unwrap().rgba, vec![255; 8]);
        },
    )
    .unwrap();
    assert_eq!(calls, 1);
    d.finish().unwrap();
}
#[test]
fn pgs_palette_delta_duplicate_clear_and_equal_time_provenance() {
    let mut d = PgsDecoder::new(Default::default());
    d.push_packet(100, &basic_with_rle(2, 1, &[1, 2, 0, 0]), |_| {})
        .unwrap();
    // Same raster at the same ns is still a committed event, marked unchanged.
    d.push_packet(
        100,
        &join(&[pcs(0, false, 0, &[reference(1, 0, 0, 2, 1, None)]), end()]),
        |image| {
            assert!(image.unchanged);
            assert_eq!(image.previous_timestamp_ns, Some(100));
        },
    )
    .unwrap();
    // Zero-object palette update retains successful composition, merges slot1.
    d.push_packet(
        101,
        &join(&[
            pcs(0, true, 0, &[]),
            pds(0, &[[1, 16, 128, 128, 255]]),
            end(),
        ]),
        |image| {
            assert!(image.palette_update);
            assert_eq!(image.objects.len(), 1);
            assert!(!image.unchanged);
            assert_eq!(
                image.image.as_ref().unwrap().rgba,
                vec![0, 0, 0, 255, 0, 0, 0, 128]
            );
        },
    )
    .unwrap();
    // No update flag is a clear, not a stale reuse, and doesn't require palette99.
    d.push_packet(102, &join(&[pcs(0, false, 99, &[]), end()]), |image| {
        assert!(image.image.is_none());
        assert!(image.objects.is_empty());
        assert_eq!(image.previous_timestamp_ns, Some(101));
    })
    .unwrap();
    d.finish().unwrap();
}
#[test]
fn pgs_palette_update_requires_successful_same_epoch_state() {
    for state in [0, 0x40, 0x80] {
        fails(&join(&[pcs(state, true, 0, &[]), end()]));
    }
    let mut d = PgsDecoder::new(Default::default());
    d.push_packet(1, &basic(), |_| {}).unwrap();
    assert!(
        d.push_packet(2, &join(&[pcs(0x80, true, 0, &[]), end()]), |_| panic!())
            .is_err()
    );
    assert_eq!(
        d.push_packet(3, &basic(), |_| panic!()),
        Err(PgsError::Poisoned)
    );
}
#[test]
fn pgs_crop_placement_window_clip_and_multiple_objects_alpha_order() {
    let packet = join(&[
        pcs(
            0x80,
            false,
            0,
            &[
                reference(1, 0, 0x80, 3, 2, Some([1, 0, 3, 1])),
                reference(2, 0, 0, 4, 2, None),
            ],
        ),
        wds(&[(0, [4, 2, 2, 1])]),
        pds(0, &[[1, 235, 128, 128, 255], [2, 16, 128, 128, 128]]),
        ods(1, 0, 0xc0, Some((4, 1, 6)), &[2, 1, 1, 1, 0, 0]),
        ods(2, 0, 0xc0, Some((1, 1, 3)), &[2, 0, 0]),
        end(),
    ]);
    let mut d = PgsDecoder::new(Default::default());
    d.push_packet(0, &packet, |image| {
        let i = image.image.as_ref().unwrap();
        assert_eq!(
            i.rect,
            Rect {
                x: 4,
                y: 2,
                width: 2,
                height: 1
            }
        );
        assert_eq!(i.rgba, vec![127, 127, 127, 255, 255, 255, 255, 255]);
    })
    .unwrap();
}
#[test]
fn pgs_transparent_slots_reserved_255_and_defined_zero() {
    // Four pixels:255,42,zero-run1,255,EOL.
    let packet = join(&[
        pcs(0x80, false, 0, &[reference(1, 0, 0, 0, 0, None)]),
        wds(&[(0, [0, 0, 16, 8])]),
        pds(0, &[[0, 235, 128, 128, 255], [255, 235, 128, 128, 255]]),
        ods(1, 0, 0xc0, Some((4, 1, 7)), &[255, 42, 0, 1, 255, 0, 0]),
        end(),
    ]);
    assert_eq!(
        decode(&packet).unwrap(),
        vec![0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 0]
    );
    assert!(
        decode(&basic_with_rle(2, 1, &[255, 42, 0, 0]))
            .unwrap()
            .is_empty()
    );
}
#[test]
fn pgs_color_neutral_endpoints_clamping_and_straight_alpha() {
    let packet = join(&[
        pcs(0x80, false, 0, &[reference(1, 0, 0, 0, 0, None)]),
        wds(&[(0, [0, 0, 16, 8])]),
        pds(
            0,
            &[
                [1, 16, 128, 128, 255],
                [2, 235, 128, 128, 255],
                [3, 255, 255, 255, 128],
            ],
        ),
        ods(1, 0, 0xc0, Some((3, 1, 5)), &[1, 2, 3, 0, 0]),
        end(),
    ]);
    assert_eq!(
        decode(&packet).unwrap(),
        vec![0, 0, 0, 255, 255, 255, 255, 255, 255, 183, 255, 128]
    );
}
#[test]
fn pgs_object_replacement_version_reuse_and_multiple_palettes() {
    let mut d = PgsDecoder::new(Default::default());
    d.push_packet(0, &basic(), |_| {}).unwrap();
    let packet = join(&[
        pcs(0, false, 1, &[reference(1, 0, 0, 0, 0, None)]),
        pds(1, &[[2, 235, 128, 128, 255]]),
        ods(1, 1, 0xc0, Some((1, 1, 3)), &[2, 0, 0]),
        end(),
        pcs(0, false, 0, &[reference(1, 0, 0, 0, 0, None)]),
        end(),
    ]);
    let mut calls = 0;
    d.push_packet(1, &packet, |image| {
        let pixels = &image.image.as_ref().unwrap().rgba;
        assert_eq!(
            pixels,
            if calls == 0 {
                &[255, 255, 255, 255]
            } else {
                &[0, 0, 0, 128]
            }
        );
        calls += 1;
    })
    .unwrap();
    assert_eq!(calls, 2);
}
#[test]
fn pgs_epoch_drops_objects_palettes_and_windows() {
    for parts in [
        vec![],
        vec![pds(0, &[[1, 235, 128, 128, 255]])],
        vec![
            pds(0, &[[1, 235, 128, 128, 255]]),
            ods(1, 0, 0xc0, Some((2, 1, 4)), &[1, 1, 0, 0]),
        ],
    ] {
        let mut d = PgsDecoder::new(Default::default());
        d.push_packet(0, &basic(), |_| {}).unwrap();
        let mut packet = pcs(0x80, false, 0, &[reference(1, 0, 0, 0, 0, None)]);
        packet.extend(parts.concat());
        packet.extend(end());
        assert!(d.push_packet(1, &packet, |_| panic!()).is_err());
    }
}
#[test]
fn pgs_malformed_rle_is_not_padded_clipped_or_partial_success() {
    for rle in [
        vec![],
        vec![1],
        vec![1, 0, 0],
        vec![1, 1],
        vec![1, 1, 1, 0, 0],
        vec![0, 0x83, 1, 0, 0],
        vec![0, 0x80, 1],
        vec![0, 0x40],
        vec![1, 1, 0, 0, 0],
        vec![1, 1, 0, 0, 0, 0],
    ] {
        fails(&basic_with_rle(2, 1, &rle));
    }
}
#[test]
fn pgs_bad_segment_lengths_unknown_types_flags_and_sup_headers() {
    for packet in [
        vec![],
        vec![0x16],
        vec![0x16, 0],
        vec![0x16, 0, 11, 0],
        b"PG00000000000".to_vec(),
        join(&[pcs(0x80, false, 0, &[]), seg(0x99, &[])]),
        join(&[pcs(0x80, false, 0, &[]), seg(0x80, &[1])]),
    ] {
        fails(&packet);
    }
    for state in [0x41, 1, 0xc0] {
        assert_eq!(
            decode(&pcs(state, false, 0, &[])),
            Err(PgsError::UnsupportedCompositionState(state))
        );
    }
    let mut packet = pcs(0x80, false, 0, &[]);
    packet[11] = 1;
    fails(&packet);
    fails(&join(&[
        pcs(0x80, false, 0, &[reference(1, 0, 1, 0, 0, None)]),
        end(),
    ]));
    fails(&join(&[
        pcs(0x80, false, 0, &[]),
        pds(0, &[[1, 16, 128, 128, 255], [1, 16, 128, 128, 255]]),
        end(),
    ]));
    fails(&join(&[
        pcs(0x80, false, 0, &[]),
        seg(0x14, &[0, 0, 1]),
        end(),
    ]));
}
#[test]
fn pgs_fragment_length_order_id_version_and_flags_rejected() {
    let first = ods(1, 1, 0x80, Some((2, 1, 4)), &[1]);
    for parts in [
        vec![ods(1, 1, 0x40, None, &[1, 0, 0])],
        vec![first.clone(), ods(2, 1, 0x40, None, &[1, 0, 0])],
        vec![first.clone(), ods(1, 2, 0x40, None, &[1, 0, 0])],
        vec![first.clone(), first.clone()],
        vec![first.clone(), ods(1, 1, 0x40, None, &[0, 0])],
        vec![first, ods(1, 1, 0x40, None, &[1, 1, 0, 0])],
        vec![ods(1, 1, 0x80, Some((2, 1, 4)), &[1, 1, 0, 0])],
        vec![ods(1, 1, 0xc1, Some((2, 1, 4)), &[1, 1, 0, 0])],
        vec![seg(0x15, &[0, 1, 1, 0xc0, 0, 0, 3, 0, 2, 0, 1])],
    ] {
        let mut packet = pcs(0x80, false, 0, &[]);
        packet.extend(parts.concat());
        packet.extend(end());
        fails(&packet);
    }
}
#[test]
fn pgs_partial_eof_new_pcs_and_orphan_end_fail_and_poison() {
    for packet in [
        pcs(0x80, false, 0, &[]),
        join(&[
            pcs(0x80, false, 0, &[]),
            ods(1, 0, 0x80, Some((2, 1, 4)), &[1]),
        ]),
    ] {
        let mut d = PgsDecoder::new(Default::default());
        d.push_packet(0, &packet, |_| panic!()).unwrap();
        assert_eq!(d.finish(), Err(PgsError::IncompleteDisplaySet));
        assert_eq!(
            d.push_packet(1, &basic(), |_| panic!()),
            Err(PgsError::Poisoned)
        );
    }
    fails(&end());
    fails(&join(&[pcs(0x80, false, 0, &[]), pcs(0, false, 0, &[])]));
}
#[test]
fn pgs_timestamp_regression_and_maximum_checked_at_nanoseconds() {
    let mut d = PgsDecoder::new(Default::default());
    d.push_packet(1_000_999, &basic(), |_| {}).unwrap();
    assert_eq!(
        d.push_packet(1_000_998, &basic(), |_| panic!()),
        Err(PgsError::InvalidTimestamp)
    );
    let mut d = PgsDecoder::new(PgsLimits {
        max_timestamp_ns: 3,
        ..Default::default()
    });
    d.push_packet(3, &basic(), |_| {}).unwrap();
    assert_eq!(
        d.push_packet(4, &basic(), |_| panic!()),
        Err(PgsError::InvalidTimestamp)
    );
}
#[test]
fn pgs_missing_references_invalid_crops_windows_canvas_are_errors() {
    fails(&join(&[
        pcs(0x80, false, 99, &[reference(1, 0, 0, 0, 0, None)]),
        end(),
    ]));
    for crop in [Some([1, 0, 2, 1]), Some([0, 0, 0, 1]), Some([0, 1, 1, 1])] {
        let mut d = PgsDecoder::new(Default::default());
        d.push_packet(0, &basic(), |_| {}).unwrap();
        assert!(
            d.push_packet(
                1,
                &join(&[
                    pcs(0, false, 0, &[reference(1, 0, 0x80, 0, 0, crop)]),
                    end()
                ]),
                |_| panic!()
            )
            .is_err()
        );
    }
    for windows in [
        vec![(0, [15, 0, 2, 1])],
        vec![(0, [0, 0, 0, 1])],
        vec![(0, [0, 0, 1, 1]), (0, [1, 0, 1, 1])],
    ] {
        fails(&join(&[pcs(0x80, false, 0, &[]), wds(&windows), end()]));
    }
    let mut packet = basic();
    packet[3..5].copy_from_slice(&0u16.to_be_bytes());
    fails(&packet);
}
#[test]
fn pgs_each_allocation_and_work_limit_fails_closed() {
    let b = basic();
    let defaults = PgsLimits::default();
    let cases = [
        PgsLimits {
            packet_bytes: b.len() - 1,
            ..defaults
        },
        PgsLimits {
            display_set_bytes: b.len() - 1,
            ..defaults
        },
        PgsLimits {
            object_bytes: 3,
            ..defaults
        },
        PgsLimits {
            object_state_bytes: 5,
            ..defaults
        },
        PgsLimits {
            raster_bytes: 7,
            ..defaults
        },
        PgsLimits {
            max_width: 15,
            ..defaults
        },
        PgsLimits {
            max_height: 7,
            ..defaults
        },
        PgsLimits {
            pixels: 127,
            ..defaults
        },
        PgsLimits {
            objects: 0,
            ..defaults
        },
        PgsLimits {
            palettes: 0,
            ..defaults
        },
        PgsLimits {
            windows: 0,
            ..defaults
        },
        PgsLimits {
            composition_objects: 0,
            ..defaults
        },
        PgsLimits {
            segments_per_display: 4,
            ..defaults
        },
        PgsLimits {
            packets: 0,
            ..defaults
        },
        PgsLimits {
            displays: 0,
            ..defaults
        },
        PgsLimits {
            total_packet_bytes: b.len() - 1,
            ..defaults
        },
        PgsLimits {
            total_render_pixels: 1,
            ..defaults
        },
    ];
    for limits in cases {
        let mut d = PgsDecoder::new(limits);
        assert!(
            matches!(
                d.push_packet(0, &b, |_| panic!("budgeted display emitted")),
                Err(PgsError::BudgetExceeded(_))
            ),
            "{limits:?}"
        );
    }
    let mut d = PgsDecoder::new(PgsLimits {
        object_state_bytes: 6,
        raster_bytes: 8,
        total_render_pixels: 2,
        ..defaults
    });
    d.push_packet(0, &b, |_| {}).unwrap();
    d.finish().unwrap();
}
#[test]
fn pgs_aggregate_cache_pending_old_new_and_raster_peak_limits() {
    let reuse = join(&[pcs(0, false, 0, &[reference(1, 0, 0, 2, 1, None)]), end()]);
    for limits in [
        PgsLimits {
            raster_bytes: 15,
            ..Default::default()
        },
        PgsLimits {
            total_render_pixels: 3,
            ..Default::default()
        },
        PgsLimits {
            displays: 1,
            ..Default::default()
        },
        PgsLimits {
            packets: 1,
            ..Default::default()
        },
    ] {
        let mut d = PgsDecoder::new(limits);
        d.push_packet(0, &basic(), |_| {}).unwrap();
        assert!(matches!(
            d.push_packet(1, &reuse, |_| panic!()),
            Err(PgsError::BudgetExceeded(_))
        ));
    }
    // Existing2 indices + pending4 RLE + temporary2 indices =8; budget6 rejects.
    let mut d = PgsDecoder::new(PgsLimits {
        object_state_bytes: 6,
        ..Default::default()
    });
    d.push_packet(0, &basic(), |_| {}).unwrap();
    let packet = join(&[
        pcs(0, false, 0, &[]),
        ods(2, 0, 0xc0, Some((2, 1, 4)), &[1, 1, 0, 0]),
        end(),
    ]);
    assert!(matches!(
        d.push_packet(1, &packet, |_| panic!()),
        Err(PgsError::BudgetExceeded("object state bytes"))
    ));
    // Replacement also charges OLD cached indices until new decode succeeds.
    let mut d = PgsDecoder::new(PgsLimits {
        object_state_bytes: 6,
        ..Default::default()
    });
    d.push_packet(0, &basic(), |_| {}).unwrap();
    assert!(
        d.push_packet(
            1,
            &join(&[
                pcs(0, false, 0, &[]),
                ods(1, 1, 0xc0, Some((2, 1, 4)), &[1, 1, 0, 0]),
                end()
            ]),
            |_| panic!()
        )
        .is_err()
    );
}
#[test]
fn pgs_huge_declared_object_rejected_before_body_or_cache_allocation() {
    let packet = join(&[
        pcs(0x80, false, 0, &[]),
        seg(0x15, &[0, 1, 0, 0x80, 0xff, 0xff, 0xff, 0, 2, 0, 1]),
    ]);
    assert_eq!(
        decode(&packet),
        Err(PgsError::BudgetExceeded("compressed object bytes"))
    );
}
#[test]
fn pgs_mkv_callback_track_selection_complete_and_stopped_not_text() {
    let mut tracks = track(8, "S_HDMV/PGS", &[]);
    tracks.extend(track_kind(1, "V_MPEG4/ISO/AVC", 1, &[]));
    let data = basic();
    let clear = join(&[pcs(0, false, 0, &[]), end()]);
    let mut blocks = block(ids::SIMPLE_BLOCK, 1, 0, 0, &vec![0xff; 128 * 1024]);
    blocks.extend(block(ids::SIMPLE_BLOCK, 8, 1, 0, &data));
    blocks.extend(block(ids::SIMPLE_BLOCK, 8, 2, 0, &clear));
    let bytes = container(&tracks, 1, &cluster(1_000_000, &blocks));
    let mut times = Vec::new();
    let summary = scan_pgs(
        Cursor::new(bytes.clone()),
        8,
        StreamingLimits::default(),
        PgsLimits::default(),
        |d| {
            times.push(d.timestamp_ns);
            ControlFlow::Continue(())
        },
    )
    .unwrap();
    assert_eq!(times, [1_000_001, 1_000_002]);
    assert_eq!(summary.completion, ScanCompletion::Complete);
    assert_eq!(summary.packets, 2);
    assert!(matches!(
        tvmatch::media::extract_subtitles(Cursor::new(bytes.clone()), Some(8)),
        Err(MediaError::UnsupportedTrack(8))
    ));
    let summary = scan_pgs(
        Cursor::new(bytes),
        8,
        StreamingLimits::default(),
        PgsLimits::default(),
        |_| ControlFlow::Break(()),
    )
    .unwrap();
    assert_eq!(summary.completion, ScanCompletion::Stopped);
    assert_eq!(summary.displays, 1);
}
#[test]
fn pgs_mkv_separate_packet_limit_and_partial_suffix_propagation() {
    let packet = basic_with_rle(2, 1, &[1, 1, 0, 0]);
    let bytes = container(
        &track(8, "S_HDMV/PGS", &[]),
        1,
        &cluster(0, &block(ids::SIMPLE_BLOCK, 8, 0, 0, &packet)),
    );
    assert!(matches!(
        scan_pgs(
            Cursor::new(bytes),
            8,
            StreamingLimits::default(),
            PgsLimits {
                packet_bytes: packet.len() - 1,
                ..Default::default()
            },
            |_| panic!()
        ),
        Err(MediaError::Pgs(PgsError::BudgetExceeded(_)))
    ));
    let mut b = basic();
    b.extend(pcs(0, false, 0, &[]));
    let bytes = container(
        &track(8, "S_HDMV/PGS", &[]),
        1,
        &cluster(0, &block(ids::SIMPLE_BLOCK, 8, 0, 0, &b)),
    );
    let mut calls = 0;
    assert!(matches!(
        scan_pgs(
            Cursor::new(bytes),
            8,
            StreamingLimits::default(),
            PgsLimits::default(),
            |_| {
                calls += 1;
                ControlFlow::Continue(())
            }
        ),
        Err(MediaError::Pgs(PgsError::IncompleteDisplaySet))
    ));
    assert_eq!(calls, 1); // A callback is not clean EOF evidence.
}
#[test]
fn pgs_mkv_packet_over_text_cap_is_allowed_but_never_text_extracted() {
    // Legal unused palette updates keep a single selected packet above4KiB.
    let mut packet = pcs(0x80, false, 0, &[]);
    for _ in 0..600 {
        packet.extend(pds(0, &[[1, 235, 128, 128, 255]]));
    }
    packet.extend(end());
    assert!(packet.len() > 4096);
    let bytes = container(
        &track(8, "S_HDMV/PGS", &[]),
        1,
        &cluster(0, &block(ids::SIMPLE_BLOCK, 8, 0, 0, &packet)),
    );
    let summary = scan_pgs(
        Cursor::new(bytes),
        8,
        StreamingLimits::default(),
        PgsLimits::default(),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    assert_eq!(summary.completion, ScanCompletion::Complete);
}

#[test]
fn pgs_true_fourteen_bit_run_and_multiple_pending_objects() {
    let rle = [0, 0xc1, 0x2c, 1, 0, 0]; // colored run300, not just long-form short run
    let image = decode(&basic_with_rle(300, 1, &rle)).unwrap();
    assert_eq!(image, vec![255; 14 * 4]); // PCSx2, WDS clips at16
    let packet = join(&[
        pcs(
            0x80,
            false,
            0,
            &[
                reference(1, 0, 0, 0, 0, None),
                reference(2, 0, 0, 1, 0, None),
            ],
        ),
        wds(&[(0, [0, 0, 16, 8])]),
        pds(0, &[[1, 235, 128, 128, 255]]),
        ods(1, 0, 0x80, Some((1, 1, 3)), &[1]),
        ods(2, 1, 0x80, Some((1, 1, 3)), &[1]),
        ods(2, 1, 0x40, None, &[0, 0]),
        ods(1, 0, 0x40, None, &[0, 0]),
        end(),
    ]);
    assert_eq!(decode(&packet).unwrap(), vec![255; 8]);
}
#[test]
fn pgs_two_partial_alpha_layers_use_straight_alpha_source_over() {
    let packet = join(&[
        pcs(
            0x80,
            false,
            0,
            &[
                reference(1, 0, 0, 0, 0, None),
                reference(2, 0, 0, 0, 0, None),
            ],
        ),
        wds(&[(0, [0, 0, 16, 8])]),
        pds(0, &[[1, 235, 128, 128, 128], [2, 16, 128, 128, 128]]),
        ods(1, 0, 0xc0, Some((1, 1, 3)), &[1, 0, 0]),
        ods(2, 0, 0xc0, Some((1, 1, 3)), &[2, 0, 0]),
        end(),
    ]);
    assert_eq!(decode(&packet).unwrap(), vec![85, 85, 85, 192]);
}
#[test]
fn pgs_live_id_and_pending_display_limits_do_not_reset_per_segment() {
    let initial = basic();
    for (limits, extra) in [
        (
            PgsLimits {
                objects: 1,
                ..Default::default()
            },
            ods(2, 0, 0xc0, Some((1, 1, 3)), &[1, 0, 0]),
        ),
        (
            PgsLimits {
                palettes: 1,
                ..Default::default()
            },
            pds(1, &[[1, 235, 128, 128, 255]]),
        ),
    ] {
        let mut d = PgsDecoder::new(limits);
        d.push_packet(0, &initial, |_| {}).unwrap();
        assert!(matches!(
            d.push_packet(
                1,
                &join(&[pcs(0, false, 0, &[]), extra, end()]),
                |_| panic!()
            ),
            Err(PgsError::BudgetExceeded(_))
        ));
    }
    let pre = pcs(0x80, false, 0, &[]);
    let mut d = PgsDecoder::new(PgsLimits {
        display_set_bytes: pre.len() + 2,
        ..Default::default()
    });
    d.push_packet(0, &pre, |_| {}).unwrap();
    assert_eq!(
        d.push_packet(1, &end(), |_| panic!()),
        Err(PgsError::BudgetExceeded("display set bytes"))
    );
}
#[test]
fn pgs_every_truncated_prefix_rejected_and_bounded_byte_mutations_do_not_panic() {
    let packet = basic();
    for len in 0..packet.len() {
        assert!(decode(&packet[..len]).is_err(), "prefix {len}");
    }
    for index in 0..packet.len() {
        for byte in [0, 1, 0x40, 0x80, 0xff] {
            let mut mutated = packet.clone();
            mutated[index] = byte;
            let _ = decode(&mutated); // mutations can be valid; only no-panic is asserted
        }
    }
}
