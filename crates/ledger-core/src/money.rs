//! Integer money math (Spec 01 §2/§8). All rounding in the system happens here,
//! half-away-from-zero, at exactly the points Spec 01 §8 names.

/// num/den rounded half-away-from-zero. i128 intermediates: qty × price × bp
/// products overflow i64 long before they overflow i128.
pub fn round_ratio(num: i128, den: i128) -> i64 {
    debug_assert!(den > 0);
    let neg = num < 0;
    let n = num.unsigned_abs();
    let d = den.unsigned_abs();
    let q = n / d;
    let q = if (n % d) * 2 >= d { q + 1 } else { q };
    let q = q as i64;
    if neg { -q } else { q }
}

/// Invoice/bill line net (Spec 01 §6.1, rounding point 1):
/// qty_milli × unit_price × (10000 − discount_bp) / (1000 × 10000)
pub fn line_net(quantity_milli: i64, unit_price_kobo: i64, discount_bp: i64) -> i64 {
    round_ratio(
        quantity_milli as i128 * unit_price_kobo as i128 * (10_000 - discount_bp) as i128,
        1_000 * 10_000,
    )
}

/// VAT on a rounded net (rounding point 2).
pub fn vat_of(net_kobo: i64, rate_bp: i64) -> i64 {
    round_ratio(net_kobo as i128 * rate_bp as i128, 10_000)
}

/// WHT on an ex-VAT base (rounding point 3).
pub fn wht_of(base_kobo: i64, rate_bp: i64) -> i64 {
    round_ratio(base_kobo as i128 * rate_bp as i128, 10_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_half_away_from_zero() {
        assert_eq!(round_ratio(5, 10), 1); // 0.5 -> 1
        assert_eq!(round_ratio(-5, 10), -1); // -0.5 -> -1
        assert_eq!(round_ratio(4, 10), 0);
        assert_eq!(round_ratio(15, 10), 2);
    }

    #[test]
    fn line_net_matches_spec_formula() {
        // 2.5 units × ₦1,500.00 × 10% discount = ₦3,375.00
        assert_eq!(line_net(2_500, 150_000, 1_000), 337_500);
        // Fractional kobo: 3 × ₦0.05 with 33.33% discount = 0.100005 -> ₦0.10
        assert_eq!(line_net(3_000, 5, 3_333), 10);
    }

    #[test]
    fn vat_7_5_percent() {
        assert_eq!(vat_of(100_000_00, 750), 7_500_00);
        assert_eq!(vat_of(1, 750), 0); // 0.075 kobo rounds down
        assert_eq!(vat_of(7, 750), 1); // 0.525 kobo rounds up
    }
}
