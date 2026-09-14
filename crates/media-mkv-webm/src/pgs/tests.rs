use super::*;
// Original tiny rasters; no private subtitles or external decoder fixtures.
fn segment(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut p = vec![kind];
    p.extend((body.len() as u16).to_be_bytes());
    p.extend(body);
    p
}
fn pcs(state: u8) -> Vec<u8> {
    segment(
        0x16,
        &[
            0, 16, 0, 8, 0x10, 0, 1, state, 0, 0, 1, 0, 1, 0, 0, 0, 2, 0, 1,
        ],
    )
}
fn parts(state: u8, y: u8) -> Vec<Vec<u8>> {
    vec![
        pcs(state),
        segment(0x17, &[1, 0, 0, 0, 0, 0, 0, 16, 0, 8]),
        segment(0x14, &[0, 0, 1, y, 128, 128, 255]),
        segment(0x15, &[0, 1, 0, 0xc0, 0, 0, 8, 0, 2, 0, 1, 1, 1, 0, 0]),
        segment(0x80, &[]),
    ]
}
#[test]
fn acquisition_is_self_contained_then_normal_reuses_refreshed_resources() {
    for initial in [None, Some(0x80), Some(0x40)] {
        let mut d = PgsDecoder::new(Default::default());
        if let Some(state) = initial {
            d.push_packet(10, &parts(state, 235).concat(), |_| {})
                .unwrap();
        }
        d.push_packet(20, &parts(0x40, 16).concat(), |display| {
            assert_eq!(display.composition_state, 0x40);
            assert_eq!(display.previous_timestamp_ns, initial.map(|_| 10));
            assert_eq!(
                display.image.as_ref().unwrap().rgba,
                vec![0, 0, 0, 255, 0, 0, 0, 255]
            );
        })
        .unwrap();
        d.push_packet(30, &[pcs(0), segment(0x80, &[])].concat(), |display| {
            assert!(display.unchanged);
            assert_eq!(display.previous_timestamp_ns, Some(20));
        })
        .unwrap();
        d.finish().unwrap();
    }
}
#[test]
fn acquisition_never_reuses_stale_palette_object_window_or_composition() {
    for omitted in 1..=3 {
        let mut d = PgsDecoder::new(Default::default());
        d.push_packet(1, &parts(0x80, 235).concat(), |_| {})
            .unwrap();
        let mut refresh = parts(0x40, 235);
        refresh.remove(omitted);
        assert!(
            d.push_packet(2, &refresh.concat(), |_| panic!("stale resource accepted"))
                .is_err()
        );
        assert_eq!(d.finish(), Err(PgsError::Poisoned));
    }
    let mut d = PgsDecoder::new(Default::default());
    d.push_packet(1, &parts(0x80, 235).concat(), |_| {})
        .unwrap();
    let update = segment(0x16, &[0, 16, 0, 8, 0x10, 0, 2, 0x40, 0x80, 0, 0]);
    assert!(
        d.push_packet(2, &[update, segment(0x80, &[])].concat(), |_| panic!())
            .is_err()
    );
}
#[test]
fn acquisition_is_not_epoch_canvas_reset_and_does_not_hide_incomplete_state() {
    let mut d = PgsDecoder::new(Default::default());
    d.push_packet(1, &parts(0x80, 235).concat(), |_| {})
        .unwrap();
    let mut refresh = parts(0x40, 235);
    refresh[0][4] = 32;
    assert_eq!(
        d.push_packet(2, &refresh.concat(), |_| panic!()),
        Err(PgsError::Malformed("canvas changed without epoch"))
    );
    let mut d = PgsDecoder::new(Default::default());
    d.push_packet(1, &pcs(0x80), |_| panic!()).unwrap();
    assert_eq!(
        d.push_packet(2, &parts(0x40, 235).concat(), |_| panic!()),
        Err(PgsError::IncompleteDisplaySet)
    );
    for state in [1, 0x41, 0x7f, 0x81, 0xc0, 0xff] {
        let mut d = PgsDecoder::new(Default::default());
        assert_eq!(
            d.push_packet(1, &pcs(state), |_| panic!()),
            Err(PgsError::UnsupportedCompositionState(state))
        );
    }
}
#[test]
fn acquisition_preserves_cumulative_budgets_and_poisons_malformed_packet_tail() {
    for limits in [
        PgsLimits {
            total_render_pixels: 2,
            ..Default::default()
        },
        PgsLimits {
            packets: 1,
            ..Default::default()
        },
        PgsLimits {
            displays: 1,
            ..Default::default()
        },
        PgsLimits {
            total_packet_bytes: parts(0x80, 235).concat().len(),
            ..Default::default()
        },
    ] {
        let mut d = PgsDecoder::new(limits);
        d.push_packet(1, &parts(0x80, 235).concat(), |_| {})
            .unwrap();
        assert!(matches!(
            d.push_packet(2, &parts(0x40, 235).concat(), |_| panic!()),
            Err(PgsError::BudgetExceeded(_))
        ));
    }
    let mut d = PgsDecoder::new(Default::default());
    let mut packet = parts(0x40, 235).concat();
    packet.push(0x14);
    let mut provisional = 0;
    assert!(d.push_packet(1, &packet, |_| provisional += 1).is_err());
    assert_eq!(provisional, 1);
    assert_eq!(d.finish(), Err(PgsError::Poisoned));
}
