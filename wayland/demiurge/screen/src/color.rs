// What a monitor takes in color, from its EDID, and the HDR10 metadata a
// connector carries to it. The EDID's base block gives the primaries and
// white point; a CTA-861 extension's HDR static metadata block gives the
// transfer functions and luminance, its colorimetry block BT.2020.

/// A monitor's color volume and HDR10 support, as its EDID gives them.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayColor {
    /// Red, green, blue and white as CIE 1931 xy times 1,000,000.
    pub primaries: [i32; 8],
    /// SMPTE ST 2084 (PQ) among the transfer functions it takes.
    pub pq: bool,
    /// BT.2020 RGB among the colorimetries it takes.
    pub bt2020: bool,
    /// The luminance it wants content within: Min in 0.0001 cd/m², max in
    /// cd/m².
    pub luminance: Option<(u32, u32)>,
}

impl DisplayColor {
    /// Whether it takes HDR10: PQ over BT.2020.
    pub fn hdr10(&self) -> bool {
        self.pq && self.bt2020
    }
}

const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
const CTA: u8 = 0x02;
const EXTENDED: u8 = 7;
const COLORIMETRY: u8 = 5;
const HDR_STATIC_METADATA: u8 = 6;
const EOTF_ST2084: u8 = 1 << 2;
const BT2020_RGB: u8 = 1 << 7;

/// Read a monitor's color from its EDID; None for bytes that are not one.
pub fn parse(edid: &[u8]) -> Option<DisplayColor> {
    if edid.len() < 128 || edid[..8] != HEADER {
        return None;
    }
    // Ten-bit chromaticities, their two low bits packed into bytes 25 and
    // 26: Red and green, then blue and white.
    let low = |byte: u8, shift: u32| ((byte >> shift) & 3) as i64;
    let xy = |high: u8, low: i64| ((((high as i64) << 2) | low) * 1_000_000 / 1024) as i32;
    let (rg, bw) = (edid[25], edid[26]);
    let primaries = [
        xy(edid[27], low(rg, 6)),
        xy(edid[28], low(rg, 4)),
        xy(edid[29], low(rg, 2)),
        xy(edid[30], low(rg, 0)),
        xy(edid[31], low(bw, 6)),
        xy(edid[32], low(bw, 4)),
        xy(edid[33], low(bw, 2)),
        xy(edid[34], low(bw, 0)),
    ];
    let mut color = DisplayColor { primaries, pq: false, bt2020: false, luminance: None };
    for block in edid[128..].chunks_exact(128).filter(|b| b[0] == CTA) {
        // Data blocks run from byte 4 to the detailed timings' offset.
        let end = (block[2] as usize).clamp(4, 127);
        let mut at = 4;
        while at < end {
            let (tag, len) = (block[at] >> 5, (block[at] & 31) as usize);
            let body = &block[(at + 1).min(end)..(at + 1 + len).min(end)];
            at += 1 + len;
            if tag != EXTENDED || body.len() < 2 {
                continue;
            }
            match body[0] {
                COLORIMETRY => color.bt2020 |= body[1] & BT2020_RGB != 0,
                HDR_STATIC_METADATA => {
                    color.pq |= body[1] & EOTF_ST2084 != 0;
                    // Code values: max = 50 * 2^(v/32) cd/m², min = max *
                    // (v/255)^2 / 100.
                    if let Some(&max) = body.get(3) {
                        let max = 50.0 * 2f64.powf(max as f64 / 32.0);
                        let min = body.get(5).map_or(0.0, |&v| max * (v as f64 / 255.0).powi(2) / 100.0);
                        color.luminance = Some(((min * 10_000.0).round() as u32, max.round() as u32));
                    }
                }
                _ => {}
            }
        }
    }
    Some(color)
}

/// HDR10's static metadata as the connector's HDR_OUTPUT_METADATA carries
/// it to the monitor: The mastering display and the content's light
/// levels, PQ always.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrMetadata {
    /// The mastering display's red, green, blue and white as CIE 1931 xy
    /// times 1,000,000.
    pub primaries: [i32; 8],
    /// Its luminance: Min in 0.0001 cd/m², max in cd/m².
    pub mastering_luminance: (u32, u32),
    /// The content's brightest pixel and brightest frame average in cd/m²;
    /// 0 is unknown.
    pub max_cll: u32,
    pub max_fall: u32,
}

impl HdrMetadata {
    /// The kernel's struct hdr_output_metadata: A u32 metadata type, then
    /// the infoframe's (EOTF, descriptor type, primaries and white in units
    /// of 0.00002, luminances, light levels), padded to 32 bytes.
    pub fn blob(&self) -> [u8; 32] {
        const STATIC_METADATA_TYPE1: u32 = 0;
        const EOTF_PQ: u8 = 2;
        let mut blob = [0u8; 32];
        blob[..4].copy_from_slice(&STATIC_METADATA_TYPE1.to_ne_bytes());
        blob[4] = EOTF_PQ;
        blob[5] = STATIC_METADATA_TYPE1 as u8;
        let xy = |v: i32| (v / 20).clamp(0, 50_000) as u16;
        let clamp = |v: u32| v.min(u16::MAX as u32) as u16;
        let fields = self
            .primaries
            .map(xy)
            .into_iter()
            .chain([self.mastering_luminance.1, self.mastering_luminance.0, self.max_cll, self.max_fall].map(clamp));
        for (i, v) in fields.enumerate() {
            blob[6 + 2 * i..8 + 2 * i].copy_from_slice(&v.to_ne_bytes());
        }
        blob
    }
}
