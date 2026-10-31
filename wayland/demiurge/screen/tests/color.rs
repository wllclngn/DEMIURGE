// A monitor's color read from its EDID, and the HDR10 metadata blob the
// kernel takes for a connector, checked against the layouts the EDID and
// CTA-861 standards and the kernel's struct hdr_output_metadata give.

use demiurge_wl_screen::color::{self, HdrMetadata};

// An EDID's base block with sRGB's primaries and D65 white as ten-bit
// chromaticities (x and y times 1024), split as the base block stores
// them: The two low bits of each packed into bytes 25 and 26, the eight
// high bits in bytes 27 to 34.
fn base(extensions: u8) -> Vec<u8> {
    let mut edid = vec![0u8; 128];
    edid[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
    // Red (0.640, 0.330), green (0.300, 0.600), blue (0.150, 0.060), white
    // (0.3127, 0.3290).
    let ten_bit: [u16; 8] = [655, 338, 307, 614, 154, 61, 320, 337];
    edid[25] = ten_bit[..4].iter().fold(0, |acc, v| (acc << 2) | (v & 3) as u8);
    edid[26] = ten_bit[4..].iter().fold(0, |acc, v| (acc << 2) | (v & 3) as u8);
    for (i, v) in ten_bit.iter().enumerate() {
        edid[27 + i] = (v >> 2) as u8;
    }
    edid[126] = extensions;
    edid
}

// A CTA-861 extension holding `blocks`, data blocks with their headers.
fn cta(blocks: &[&[u8]]) -> Vec<u8> {
    let mut ext = vec![0u8; 128];
    ext[0] = 0x02;
    ext[1] = 0x03;
    let mut at = 4;
    for block in blocks {
        ext[at..at + block.len()].copy_from_slice(block);
        at += block.len();
    }
    ext[2] = at as u8;
    ext
}

// The HDR static metadata block: Extended tag 6, SDR and PQ, static
// metadata type 1, a max luminance code of 96 (50 * 2^3 = 400 cd/m²), a
// frame-average code of 64 and a min code of 51 (400 * 0.2^2 / 100 =
// 0.16 cd/m²).
const HDR_BLOCK: [u8; 7] = [0xe6, 6, 0b0101, 1, 96, 64, 51];
// The colorimetry block: Extended tag 5, BT.2020 RGB and YCC.
const COLORIMETRY_BLOCK: [u8; 4] = [0xe3, 5, 0xc0, 0];

const SRGB: [f64; 8] = [0.640, 0.330, 0.300, 0.600, 0.150, 0.060, 0.3127, 0.3290];

#[test]
fn the_base_block_gives_the_primaries_and_no_hdr() {
    let color = color::parse(&base(0)).expect("an EDID");
    for (got, want) in color.primaries.iter().zip(SRGB) {
        assert!((*got as f64 / 1e6 - want).abs() < 0.001, "{:?} is not sRGB's {:?}", color.primaries, SRGB);
    }
    assert!(!color.pq && !color.bt2020 && color.luminance.is_none(), "{:?}", color);
    assert!(!color.hdr10());
}

#[test]
fn a_cta_extension_gives_hdr10_and_the_luminance() {
    let mut edid = base(1);
    edid.extend(cta(&[&[0x43, 0x90, 0x04, 0x03], &HDR_BLOCK, &COLORIMETRY_BLOCK]));
    let color = color::parse(&edid).expect("an EDID");
    assert!(color.pq && color.bt2020 && color.hdr10(), "{:?}", color);
    assert_eq!(color.luminance, Some((1600, 400)), "{:?}", color);
}

#[test]
fn pq_without_bt2020_is_not_hdr10() {
    let mut edid = base(1);
    edid.extend(cta(&[&HDR_BLOCK]));
    let color = color::parse(&edid).expect("an EDID");
    assert!(color.pq && !color.bt2020 && !color.hdr10(), "{:?}", color);
}

#[test]
fn a_block_running_past_the_data_ends_at_it() {
    let mut edid = base(1);
    let mut ext = cta(&[&HDR_BLOCK]);
    // The block says it is longer than the data the extension holds.
    ext[4] = 0xe0 | 30;
    ext[2] = 8;
    edid.extend(ext);
    let color = color::parse(&edid).expect("an EDID");
    assert!(color.pq, "{:?}", color);
}

#[test]
fn bytes_that_are_not_an_edid_are_none() {
    assert!(color::parse(&[0u8; 128]).is_none());
    assert!(color::parse(&base(0)[..100]).is_none());
}

#[test]
fn the_metadata_blob_is_the_kernels_struct() {
    let metadata = HdrMetadata {
        primaries: [708_000, 292_000, 170_000, 797_000, 131_000, 46_000, 312_700, 329_000],
        mastering_luminance: (50, 1000),
        max_cll: 1000,
        max_fall: 400,
    };
    let blob = metadata.blob();
    let u16_at = |at: usize| u16::from_ne_bytes([blob[at], blob[at + 1]]);
    assert_eq!(blob.len(), 32, "sizeof(struct hdr_output_metadata)");
    assert_eq!(u32::from_ne_bytes(blob[..4].try_into().unwrap()), 0, "HDMI_STATIC_METADATA_TYPE1");
    assert_eq!((blob[4], blob[5]), (2, 0), "EOTF SMPTE ST 2084, static metadata type 1");
    // Red, green, blue, then white, in units of 0.00002.
    let primaries: Vec<u16> = (0..8).map(|i| u16_at(6 + 2 * i)).collect();
    assert_eq!(primaries, [35400, 14600, 8500, 39850, 6550, 2300, 15635, 16450]);
    assert_eq!(u16_at(22), 1000, "max mastering luminance in cd/m²");
    assert_eq!(u16_at(24), 50, "min mastering luminance in 0.0001 cd/m²");
    assert_eq!((u16_at(26), u16_at(28)), (1000, 400), "MaxCLL, MaxFALL");
    assert_eq!(&blob[30..], &[0, 0]);
}
