//! Post-encoding bytewise patches for `mp4-atom` v0.10 bugs.
//!
//! Test-only oracle helpers originating in media-core. Native production
//! writers encode the corrected fields directly.
//!
//! # The four bugs
//!
//! 1. **`tkhd` flags.** `mp4-atom` hardcodes `track_in_movie=false`
//!    (flags = 0x000001). Apple CoreMedia (HLS / iOS Safari) requires
//!    bit 0 (`track_enabled`) and bit 1 (`track_in_movie`) both set,
//!    flags = 0x000003.
//!
//! 2. **`dref url` `self_contained` flag.** `mp4-atom`'s `ext!` macro
//!    maps `self_contained` to bit 1 (flags = 0x000002); ISO/IEC
//!    14496-12 puts it on bit 0 (flags = 0x000001). Without bit 0,
//!    Apple CoreMedia thinks media data is external and can't find
//!    samples in `mdat` ("Error injecting segment data").
//!
//! 3. **`esds` descriptor lengths.** `mp4-atom` emits compact
//!    descriptor length encoding (e.g. `03 19`); Apple CoreMedia
//!    only accepts the four-byte expandable form (`03 80 80 80 19`).
//!    `patch_esds_descriptor_lengths` replaces the existing `esds`
//!    box with a freshly built one using expandable lengths, then
//!    walks up the box hierarchy fixing ancestor box sizes.
//!
//! 4. **`tfhd default-base-is-moof`** (0x020000). Not exposed by
//!    `mp4-atom`'s `Tfhd` struct. CMAF / Apple HLS require it for
//!    fragmented MP4 — without it CoreMedia can't resolve `trun`
//!    `data_offset` correctly.
//!
//! A fifth bug exists in `Colr::decode_body` (the `prof` and `rICC`
//! branches read with `buf.slice(n)` but never `advance(n)`, so
//! `Any::decode_atom`'s outer `has_remaining()` post-check returns
//! `UnderDecode(colr)`). It only affected the HEIF parse path,
//! which now uses [`crate::heif::read_meta`] instead of
//! `mp4_atom::Meta`.
//!
//! # Multi-track caveat
//!
//! All four patches use `buf.windows(4).position(|w| w == FOURCC)`
//! to locate the box, which finds the **first** occurrence. This is
//! correct for single-track files and for
//! HEIF (which doesn't use these `trak`-shaped boxes at all). For
//! multi-track files (video + audio), a proper box walker is
//! required; these helpers are not general-purpose production patches.

/// Patch the `tkhd` flags inside an already-encoded `moov` payload.
/// Sets bits 0 and 1 (`track_enabled | track_in_movie`); existing
/// flag bits above bit 1 are cleared.
///
/// Origin: media-core.
pub fn patch_tkhd_flags(buf: &mut [u8]) {
    if let Some(pos) = buf.windows(4).position(|w| w == b"tkhd") {
        // Box layout: [size(4)] tkhd(4) version(1) flags(3)
        // pos points at the 't' of "tkhd"; flags low byte is at pos+7.
        let flags_lo = pos + 7;
        if flags_lo < buf.len() {
            buf[flags_lo] = 0x03;
        }
    }
}

/// Patch the `dref url ` self-contained flag inside an already-encoded
/// `moov` payload. Sets bit 0 (clears bit 1 if `mp4-atom` set it).
///
/// Origin: media-core.
pub fn patch_dref_url_self_contained(buf: &mut [u8]) {
    if let Some(pos) = buf.windows(4).position(|w| w == b"url ") {
        // Box layout: [size(4)] url (4) version(1) flags(3)
        let flags_lo = pos + 7;
        if flags_lo < buf.len() {
            buf[flags_lo] = 0x01;
        }
    }
}

/// Patch `tfhd` flags to set `default-base-is-moof` (0x020000) inside
/// an already-encoded `moof` payload.
///
/// Origin: media-core.
pub fn patch_tfhd_default_base_is_moof(buf: &mut [u8]) {
    if let Some(pos) = buf.windows(4).position(|w| w == b"tfhd") {
        // Box layout: [size(4)] tfhd(4) version(1) flags(3)
        // pos+5 is the high byte of the 24-bit flags field
        // (default-base-is-moof = 0x020000 → bit 0x02 in the high byte).
        let flags_hi = pos + 5;
        if flags_hi < buf.len() {
            buf[flags_hi] |= 0x02;
        }
    }
}

/// Replace the `esds` box in `buf` with one that uses expandable
/// descriptor length encoding (4-byte `0x80 0x80 0x80 N` form),
/// required by Apple CoreMedia / iOS AVPlayer. Walks up the box
/// hierarchy and adjusts ancestor box sizes to absorb the size delta.
///
/// `audio_specific_config` is the AAC AudioSpecificConfig (typically
/// 2 bytes; up to 5 for SBR/PS profiles).
///
/// Origin: media-core.
pub fn patch_esds_descriptor_lengths(buf: &mut Vec<u8>, audio_specific_config: &[u8]) {
    let Some(esds_type_pos) = buf.windows(4).position(|w| w == b"esds") else {
        return;
    };
    let box_start = esds_type_pos - 4;

    let asc_len = audio_specific_config.len() as u8;
    // DecoderSpecificInfo: tag(1) + expandable_len(4) + asc
    let dec_spec_total = 1 + 4 + asc_len;
    // DecoderConfigDescriptor content: 13 fixed bytes + DecoderSpecificInfo
    let dec_config_content_len = 13 + dec_spec_total;
    // SLConfigDescriptor: tag(1) + expandable_len(4) + 1 byte
    let sl_total: u8 = 6;
    // ES_Descriptor content: 3 + DecoderConfigDescriptor + SLConfig
    let dec_config_total = 1 + 4 + dec_config_content_len;
    let es_content_len = 3 + dec_config_total + sl_total;

    let mut esds = Vec::new();
    esds.extend_from_slice(&[0, 0, 0, 0]); // size placeholder
    esds.extend_from_slice(b"esds");
    esds.extend_from_slice(&[0, 0, 0, 0]); // version + flags
    // ES_Descriptor (tag 0x03)
    esds.push(0x03);
    esds.extend_from_slice(&[0x80, 0x80, 0x80, es_content_len]);
    esds.extend_from_slice(&[0x00, 0x01]); // ES_ID = 1
    esds.push(0x00); // stream priority
    // DecoderConfigDescriptor (tag 0x04)
    esds.push(0x04);
    esds.extend_from_slice(&[0x80, 0x80, 0x80, dec_config_content_len]);
    esds.push(0x40); // objectTypeIndication = AAC
    esds.push(0x15); // streamType = audio
    esds.extend_from_slice(&[0x00, 0x00, 0x00]); // bufferSizeDB
    esds.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // maxBitrate
    esds.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // avgBitrate
    // DecoderSpecificInfo (tag 0x05) — wraps the AudioSpecificConfig
    esds.push(0x05);
    esds.extend_from_slice(&[0x80, 0x80, 0x80, asc_len]);
    esds.extend_from_slice(audio_specific_config);
    // SLConfigDescriptor (tag 0x06)
    esds.push(0x06);
    esds.extend_from_slice(&[0x80, 0x80, 0x80, 0x01]);
    esds.push(0x02); // predefined = 2 (MP4)

    let box_size = esds.len() as u32;
    esds[0..4].copy_from_slice(&box_size.to_be_bytes());

    let old_box_size =
        u32::from_be_bytes(buf[box_start..box_start + 4].try_into().unwrap()) as usize;
    let box_end = box_start + old_box_size;

    let size_delta = esds.len() as i32 - old_box_size as i32;
    buf.splice(box_start..box_end, esds);

    // Walk up parents and absorb the size delta.
    for tag in [
        b"mp4a", b"stsd", b"stbl", b"minf", b"mdia", b"trak", b"moov",
    ] {
        if let Some(pos) = buf.windows(4).position(|w| w == tag) {
            let size_pos = pos - 4;
            let old = u32::from_be_bytes(buf[size_pos..size_pos + 4].try_into().unwrap());
            let new = (old as i32 + size_delta) as u32;
            buf[size_pos..size_pos + 4].copy_from_slice(&new.to_be_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-encoded `tkhd` box with `mp4-atom`'s buggy default
    /// (flags = 0x000001 = track_enabled only).
    fn fake_tkhd(flags_lo: u8) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 16]); // size = 16 (header only — fixture)
        v.extend_from_slice(b"tkhd");
        v.push(0); // version
        v.extend_from_slice(&[0, 0, flags_lo]); // flags
        v.extend_from_slice(&[0; 4]); // padding to size
        v
    }

    #[test]
    fn tkhd_flags_patched() {
        let mut buf = fake_tkhd(0x01);
        patch_tkhd_flags(&mut buf);
        // Locate the flags low byte and confirm it became 0x03.
        let pos = buf.windows(4).position(|w| w == b"tkhd").unwrap();
        assert_eq!(buf[pos + 7], 0x03);
    }

    #[test]
    fn tkhd_patch_idempotent() {
        let mut buf = fake_tkhd(0x03);
        patch_tkhd_flags(&mut buf);
        let pos = buf.windows(4).position(|w| w == b"tkhd").unwrap();
        assert_eq!(buf[pos + 7], 0x03);
    }

    #[test]
    fn tkhd_patch_no_op_when_box_absent() {
        let mut buf = b"\x00\x00\x00\x10ftypmp42\x00\x00\x00\x00".to_vec();
        let before = buf.clone();
        patch_tkhd_flags(&mut buf);
        assert_eq!(buf, before);
    }

    fn fake_url(flags_lo: u8) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 12]); // size
        v.extend_from_slice(b"url ");
        v.push(0); // version
        v.extend_from_slice(&[0, 0, flags_lo]); // flags
        v
    }

    #[test]
    fn url_self_contained_patched() {
        // mp4-atom emits 0x02; we want 0x01.
        let mut buf = fake_url(0x02);
        patch_dref_url_self_contained(&mut buf);
        let pos = buf.windows(4).position(|w| w == b"url ").unwrap();
        assert_eq!(buf[pos + 7], 0x01);
    }

    #[test]
    fn url_patch_clears_wrong_bit() {
        // If both bits are set, we still end up at 0x01 (bit 1 cleared).
        let mut buf = fake_url(0x03);
        patch_dref_url_self_contained(&mut buf);
        let pos = buf.windows(4).position(|w| w == b"url ").unwrap();
        assert_eq!(buf[pos + 7], 0x01);
    }

    fn fake_tfhd(flags_hi: u8) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 16]); // size
        v.extend_from_slice(b"tfhd");
        v.push(0); // version
        v.extend_from_slice(&[flags_hi, 0, 0]); // flags
        v.extend_from_slice(&[0; 4]); // padding
        v
    }

    #[test]
    fn tfhd_default_base_is_moof_set() {
        let mut buf = fake_tfhd(0x00);
        patch_tfhd_default_base_is_moof(&mut buf);
        let pos = buf.windows(4).position(|w| w == b"tfhd").unwrap();
        assert_eq!(buf[pos + 5], 0x02);
    }

    #[test]
    fn tfhd_patch_preserves_other_high_bits() {
        // Existing flag bit 0x01 (different bit) must survive.
        let mut buf = fake_tfhd(0x01);
        patch_tfhd_default_base_is_moof(&mut buf);
        let pos = buf.windows(4).position(|w| w == b"tfhd").unwrap();
        assert_eq!(buf[pos + 5], 0x03); // 0x01 | 0x02
    }

    #[test]
    fn tfhd_patch_idempotent() {
        let mut buf = fake_tfhd(0x02);
        patch_tfhd_default_base_is_moof(&mut buf);
        let pos = buf.windows(4).position(|w| w == b"tfhd").unwrap();
        assert_eq!(buf[pos + 5], 0x02);
    }

    /// Build a minimal moov > trak > mdia > minf > stbl > stsd > mp4a > esds
    /// hierarchy with a compact-length `esds` payload.
    fn fake_moov_with_compact_esds(asc: &[u8]) -> Vec<u8> {
        // Compact-length esds: tag, len_byte, payload.
        // ES_Descriptor: 03 <es_len> 00 01 00 + DecoderConfig + SLConfig
        // For the test we only need the SHAPE of an esds box that matches
        // what mp4-atom emits — exact bitstream content is irrelevant
        // because the patch REPLACES the box wholesale.
        let mut esds = Vec::new();
        esds.extend_from_slice(&[0, 0, 0, 0]); // size placeholder
        esds.extend_from_slice(b"esds");
        esds.extend_from_slice(&[0, 0, 0, 0]); // version + flags
        esds.push(0x03); // ES_Descriptor tag
        esds.push(8 + asc.len() as u8); // compact len
        esds.extend_from_slice(&[0x00, 0x01, 0x00]); // ES_ID + priority
        esds.push(0x04); // DecoderConfigDescriptor tag
        esds.push(asc.len() as u8 + 2);
        esds.push(0x40); // objectTypeIndication
        esds.push(0x05); // DecoderSpecificInfo tag
        esds.push(asc.len() as u8);
        esds.extend_from_slice(asc);
        let esds_size = esds.len() as u32;
        esds[0..4].copy_from_slice(&esds_size.to_be_bytes());

        // Wrap each ancestor with a [size(4)] [fourcc(4)] header.
        let wrap = |inner: Vec<u8>, fourcc: &[u8; 4]| -> Vec<u8> {
            let total = inner.len() + 8;
            let mut out = Vec::with_capacity(total);
            out.extend_from_slice(&(total as u32).to_be_bytes());
            out.extend_from_slice(fourcc);
            out.extend(inner);
            out
        };

        let mp4a = wrap(esds, b"mp4a");
        let stsd = wrap(mp4a, b"stsd");
        let stbl = wrap(stsd, b"stbl");
        let minf = wrap(stbl, b"minf");
        let mdia = wrap(minf, b"mdia");
        let trak = wrap(mdia, b"trak");
        wrap(trak, b"moov")
    }

    #[test]
    fn esds_patched_to_expandable_form() {
        let asc = &[0x12, 0x10]; // typical 2-byte AAC LC ASC
        let mut buf = fake_moov_with_compact_esds(asc);
        let original_len = buf.len();
        patch_esds_descriptor_lengths(&mut buf, asc);

        // Locate the new esds and verify expandable-length form.
        let pos = buf.windows(4).position(|w| w == b"esds").unwrap();
        // After "esds" + 4 version/flag bytes, the ES_Descriptor tag (0x03)
        // is immediately followed by 0x80 0x80 0x80 LEN.
        assert_eq!(buf[pos + 4 + 4], 0x03);
        assert_eq!(&buf[pos + 4 + 5..pos + 4 + 8], &[0x80, 0x80, 0x80]);

        // The patch grew the buffer (compact lengths are 1 byte; expandable
        // are 4 bytes; the new esds is larger).
        assert!(buf.len() > original_len);
    }

    #[test]
    fn esds_patch_updates_ancestor_sizes() {
        let asc = &[0x12, 0x10];
        let mut buf = fake_moov_with_compact_esds(asc);
        patch_esds_descriptor_lengths(&mut buf, asc);

        // Each ancestor box's declared size must equal its on-wire span.
        // moov is the outermost, so its size is the whole buffer.
        let moov_pos = buf.windows(4).position(|w| w == b"moov").unwrap();
        let moov_size =
            u32::from_be_bytes(buf[moov_pos - 4..moov_pos].try_into().unwrap()) as usize;
        assert_eq!(moov_size, buf.len());

        // Each child must fit inside its parent's declared span.
        for fourcc in [b"trak", b"mdia", b"minf", b"stbl", b"stsd", b"mp4a"] {
            let pos = buf.windows(4).position(|w| w == fourcc).unwrap();
            let size = u32::from_be_bytes(buf[pos - 4..pos].try_into().unwrap()) as usize;
            // Non-zero and not crazy.
            assert!(
                size > 0 && size < buf.len() + 1,
                "{:?}: size = {}",
                fourcc,
                size
            );
        }
    }

    #[test]
    fn esds_patch_no_op_when_absent() {
        let mut buf = b"\x00\x00\x00\x10ftypmp42\x00\x00\x00\x00".to_vec();
        let before = buf.clone();
        patch_esds_descriptor_lengths(&mut buf, &[0x12, 0x10]);
        assert_eq!(buf, before);
    }
}
