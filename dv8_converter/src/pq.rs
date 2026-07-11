//! SMPTE ST 2084 (PQ) transfer function math.

const M1: f64 = 2610.0 / 16384.0;
const M2: f64 = 2523.0 / 4096.0 * 128.0;
const C1: f64 = 3424.0 / 4096.0;
const C2: f64 = 2413.0 / 4096.0 * 32.0;
const C3: f64 = 2392.0 / 4096.0 * 32.0;

/// ST 2084 EOTF: normalized PQ signal [0,1] -> luminance in nits [0,10000].
pub(crate) fn pq_to_nits(e: f64) -> f64 {
    let e = e.clamp(0.0, 1.0);
    let ep = e.powf(1.0 / M2);
    let num = (ep - C1).max(0.0);
    let den = C2 - C3 * ep;
    10000.0 * (num / den).powf(1.0 / M1)
}

/// ST 2084 inverse EOTF: luminance in nits [0,10000] -> normalized PQ signal [0,1].
#[cfg(test)]
pub(crate) fn nits_to_pq(nits: f64) -> f64 {
    let y = (nits / 10000.0).clamp(0.0, 1.0);
    let ym = y.powf(M1);
    ((C1 + C2 * ym) / (1.0 + C3 * ym)).powf(M2)
}

/// Limited-range luma code value -> normalized PQ signal [0,1].
/// For 10-bit: black = 64, white = 940. Other depths scale by 2^(n-8).
pub(crate) fn code_limited_to_pq(code: f64, bit_depth: u32) -> f64 {
    let scale = f64::from(1u32 << bit_depth.saturating_sub(8));
    let black = 16.0 * scale;
    let range = 219.0 * scale;
    ((code - black) / range).clamp(0.0, 1.0)
}

/// Limited-range luma code value at the given bit depth -> nits.
#[cfg(test)]
pub(crate) fn code_limited_to_nits(code: f64, bit_depth: u32) -> f64 {
    pq_to_nits(code_limited_to_pq(code, bit_depth))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eotf_endpoints() {
        assert!(pq_to_nits(0.0).abs() < 1e-9);
        assert!((pq_to_nits(1.0) - 10000.0).abs() < 1e-6);
    }

    #[test]
    fn inverse_known_points() {
        // Well-known PQ anchor: 100 nits ~ 0.5081 PQ
        assert!((nits_to_pq(100.0) - 0.5081).abs() < 0.0005);
        // 1000 nits ~ 0.7518 PQ
        assert!((nits_to_pq(1000.0) - 0.7518).abs() < 0.0005);
    }

    #[test]
    fn round_trip() {
        // ST 2084 inverse of exactly 0 nits is ~7e-7 PQ, not 0 — use 1e-6.
        for &x in &[0.0, 0.1, 0.25, 0.5081, 0.75, 0.9, 1.0] {
            assert!((nits_to_pq(pq_to_nits(x)) - x).abs() < 1e-6, "x={x}");
        }
    }

    #[test]
    fn code_10bit_limited_range() {
        assert!(code_limited_to_nits(64.0, 10).abs() < 1e-9);
        assert!((code_limited_to_nits(940.0, 10) - 10000.0).abs() < 1e-6);
        // Below black / above white clamp
        assert!(code_limited_to_nits(0.0, 10).abs() < 1e-9);
        assert!((code_limited_to_nits(1023.0, 10) - 10000.0).abs() < 1e-6);
        // 8-bit black/white
        assert!(code_limited_to_pq(16.0, 8).abs() < 1e-12);
        assert!((code_limited_to_pq(235.0, 8) - 1.0).abs() < 1e-12);
    }
}
