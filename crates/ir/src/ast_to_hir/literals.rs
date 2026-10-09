//! Number literals, validated exactly as the Nix lexer does it.
//!
//! Verified against Nix 2.28:
//! - Integers must fit in `i64`; otherwise `invalid integer '...'`. Leading
//!   zeros are allowed (`007` is 7). A negative literal is a negation of a
//!   positive one, so `-9223372036854775808` is out of range.
//! - Floats are rounded to the nearest `f64` (ties to even). They are rejected
//!   with `invalid float '...'` when they overflow to infinity, or when they
//!   underflow: the value is inexact and below 2^-1022 even after rounding to
//!   53 bits with an unbounded exponent (IEEE 754 "tininess after rounding").
//!   So `2.2e-308` is rejected, but an exactly representable subnormal such
//!   as 2^-1074 written out in full is accepted, and so is `0.0e-400`.

/// The value of an integer literal, if it is in range.
pub(super) fn parse_int(text: &str) -> Option<i64> {
    text.parse().ok()
}

/// The value of a float literal, if Nix accepts it.
pub(super) fn parse_float(text: &str) -> Option<f64> {
    let f: f64 = text.parse().ok()?;
    if f.is_infinite() {
        return None;
    }
    if f > f64::MIN_POSITIVE {
        return Some(f);
    }
    // Zero, subnormal, or rounded up to exactly 2^-1022: decide on the exact
    // decimal value of the literal.
    let Some(literal) = Decimal::from_literal(text)? else {
        return Some(0.0);
    };
    if f == f64::MIN_POSITIVE {
        // Values in [2^-1022 - 2^-1076, 2^-1022) round to 2^-1022 with an
        // unbounded exponent, so they are not tiny; anything below is.
        let threshold = Decimal::from_multiple_of_power_of_two((1 << 54) - 1, 1076);
        return (literal >= threshold).then_some(f);
    }
    // Tiny: accepted only if exact. A positive f64 below 2^-1022 is
    // `bits * 2^-1074`.
    (f > 0.0 && literal == Decimal::from_multiple_of_power_of_two(f.to_bits(), 1074)).then_some(f)
}

/// A positive decimal number `0.d1 d2 d3 ... * 10^magnitude`.
///
/// Stored without leading or trailing zeros, so the derived ordering on
/// `(magnitude, digits)` is numeric order and equality is numeric equality.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Decimal {
    magnitude: i64,
    digits: Vec<u8>,
}

impl Decimal {
    /// `digits * 10^exp`, or `None` if `digits` is zero.
    fn new(digits: impl IntoIterator<Item = u8>, exp: i64) -> Option<Self> {
        let mut digits: Vec<u8> = digits.into_iter().skip_while(|&d| d == 0).collect();
        let trailing_zeros = digits.iter().rev().take_while(|&&d| d == 0).count();
        digits.truncate(digits.len() - trailing_zeros);
        if digits.is_empty() {
            return None;
        }
        let magnitude =
            i64::try_from(digits.len()).ok()? + i64::try_from(trailing_zeros).ok()? + exp;
        Some(Decimal { magnitude, digits })
    }

    /// The exact value of a float literal (`1.5e3`, `.5`, `1.e2`).
    ///
    /// Returns `Some(None)` for zero, and `None` if the exponent does not fit
    /// in an `i64` (the literal is then far outside the `f64` range).
    fn from_literal(text: &str) -> Option<Option<Self>> {
        let (mantissa, exp) = match text.find(['e', 'E']) {
            Some(i) => (&text[..i], &text[i + 1..]),
            None => (text, "0"),
        };
        let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let digits = int_part.bytes().chain(frac_part.bytes()).map(|b| b - b'0');
        if digits.clone().all(|d| d == 0) {
            return Some(None);
        }
        let exp = exp.parse::<i64>().ok()?;
        let exp = exp.checked_sub(i64::try_from(frac_part.len()).ok()?)?;
        Some(Decimal::new(digits, exp))
    }

    /// The exact value of `k * 2^-n`, computed as `k * 5^n * 10^-n`.
    fn from_multiple_of_power_of_two(k: u64, n: u32) -> Self {
        let digits = BigUint::from_u64(k).mul_pow5(n).to_digits();
        Decimal::new(digits, -i64::from(n)).expect("k is non-zero")
    }
}

/// Just enough of an arbitrary-precision unsigned integer for the exact
/// comparisons above: little-endian limbs in base 10^9.
struct BigUint(Vec<u32>);

impl BigUint {
    const BASE: u64 = 1_000_000_000;

    fn from_u64(mut n: u64) -> Self {
        let mut limbs = Vec::new();
        while n > 0 {
            limbs.push((n % Self::BASE) as u32);
            n /= Self::BASE;
        }
        BigUint(limbs)
    }

    fn mul_small(mut self, m: u32) -> Self {
        let mut carry = 0u64;
        for limb in &mut self.0 {
            let product = u64::from(*limb) * u64::from(m) + carry;
            *limb = (product % Self::BASE) as u32;
            carry = product / Self::BASE;
        }
        while carry > 0 {
            self.0.push((carry % Self::BASE) as u32);
            carry /= Self::BASE;
        }
        self
    }

    fn mul_pow5(mut self, n: u32) -> Self {
        // 5^13 = 1_220_703_125 fits in a u32.
        for _ in 0..n / 13 {
            self = self.mul_small(1_220_703_125);
        }
        self.mul_small(5u32.pow(n % 13))
    }

    /// Decimal digits, most significant first.
    fn to_digits(&self) -> Vec<u8> {
        let mut text = String::new();
        if let Some((last, rest)) = self.0.split_last() {
            text.push_str(&last.to_string());
            for limb in rest.iter().rev() {
                text.push_str(&format!("{limb:09}"));
            }
        }
        text.bytes().map(|b| b - b'0').collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every expectation below is what Nix 2.28 does with the same literal.

    #[test]
    fn integers() {
        assert_eq!(parse_int("0"), Some(0));
        assert_eq!(parse_int("007"), Some(7));
        assert_eq!(parse_int("9223372036854775807"), Some(i64::MAX));
        assert_eq!(parse_int("9223372036854775808"), None);
    }

    #[test]
    fn ordinary_floats() {
        assert_eq!(parse_float("1.5e3"), Some(1500.0));
        assert_eq!(parse_float("1.5e+3"), Some(1500.0));
        assert_eq!(parse_float("1.e2"), Some(100.0));
        assert_eq!(parse_float(".5"), Some(0.5));
        assert_eq!(parse_float("0.1"), Some(0.1));
        assert_eq!(parse_float("0.0"), Some(0.0));
        assert_eq!(parse_float("0.0e-400"), Some(0.0));
        assert_eq!(parse_float("1.0e-307"), Some(1e-307));
    }

    #[test]
    fn overflow() {
        assert_eq!(parse_float("1.0e308"), Some(1e308));
        assert_eq!(parse_float("1.7976931348623158e308"), Some(f64::MAX));
        assert_eq!(parse_float("1.7976931348623159e308"), None);
        assert_eq!(parse_float("1.8e308"), None);
        assert_eq!(parse_float("1.0e309"), None);
    }

    #[test]
    fn underflow() {
        assert_eq!(
            parse_float("2.2250738585072014e-308"),
            Some(f64::MIN_POSITIVE)
        );
        // Below 2^-1022 but rounds up to it with an unbounded exponent.
        assert_eq!(
            parse_float("2.22507385850720138e-308"),
            Some(f64::MIN_POSITIVE)
        );
        // Below 2^-1022 - 2^-1076: tiny.
        assert_eq!(parse_float("2.2250738585072012e-308"), None);
        assert_eq!(parse_float("2.2e-308"), None);
        assert_eq!(parse_float("1.0e-310"), None);
        assert_eq!(parse_float("4.9e-324"), None);
        assert_eq!(parse_float("1.0e-400"), None);
        assert_eq!(parse_float("1e-99999999999999999999999"), None);
    }

    #[test]
    fn underflow_threshold_is_exact() {
        let threshold = Decimal::from_multiple_of_power_of_two((1 << 54) - 1, 1076);
        let text = |d: &Decimal, last_delta: i8| {
            let mut digits = d.digits.clone();
            let last = digits.last_mut().unwrap();
            *last = u8::try_from(i16::from(*last) + i16::from(last_delta)).unwrap();
            let s: String = digits.iter().map(|d| char::from(b'0' + d)).collect();
            format!("0.{s}e{}", d.magnitude)
        };
        // Nix: the exact midpoint is accepted, one unit in the last place
        // below it is not.
        assert_eq!(parse_float(&text(&threshold, 0)), Some(f64::MIN_POSITIVE));
        assert_eq!(parse_float(&text(&threshold, -1)), None);
        assert_eq!(parse_float(&text(&threshold, 1)), Some(f64::MIN_POSITIVE));
    }

    #[test]
    fn exact_subnormals_are_accepted() {
        let min_subnormal = Decimal::from_multiple_of_power_of_two(1, 1074);
        let digits: String = min_subnormal
            .digits
            .iter()
            .map(|d| char::from(b'0' + d))
            .collect();
        let text = format!("0.{digits}e{}", min_subnormal.magnitude);
        assert_eq!(parse_float(&text), Some(f64::from_bits(1)));
    }

    #[test]
    fn biguint_digits() {
        assert_eq!(BigUint::from_u64(0).to_digits(), Vec::<u8>::new());
        assert_eq!(BigUint::from_u64(1).mul_pow5(3).to_digits(), vec![1, 2, 5]);
        assert_eq!(
            BigUint::from_u64(1_000_000_000).to_digits(),
            vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }
}
