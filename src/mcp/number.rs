//! JSON numbers read exactly from their digits. With serde_json's
//! `arbitrary_precision` a number keeps the digits it was sent with, in any
//! form, such as `4.111111111111111e15`.

use serde_json::Number;

/// Longest plain form worth writing out: past it no redaction pattern matches.
const MAX_PLAIN_LENGTH: usize = 64;

/// A number as its sign, significant digits and power of ten, so equal values
/// are equal however they are written or however wide. Zero has no sign.
#[derive(Debug, PartialEq)]
pub(crate) struct Decimal {
    negative: bool,
    digits: String,
    power: i64,
}

impl Decimal {
    pub(crate) fn of(number: &Number) -> Option<Self> {
        let text = number.to_string();
        let (mantissa, exponent) = match text.split_once(['e', 'E']) {
            Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
            None => (text.as_str(), 0),
        };
        let negative = mantissa.starts_with('-');
        let unsigned = mantissa.trim_start_matches('-');
        let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let digits = format!("{whole}{fraction}");
        let significant = digits.trim_start_matches('0').trim_end_matches('0');
        if significant.is_empty() {
            return Some(Self {
                negative: false,
                digits: String::new(),
                power: 0,
            });
        }
        let trailing_zeros = digits.trim_start_matches('0').len() - significant.len();
        let power = exponent
            .checked_sub(fraction.len() as i64)?
            .checked_add(trailing_zeros as i64)?;
        Some(Self {
            negative,
            digits: significant.to_string(),
            power,
        })
    }

    /// The number in digits with no exponent, such as `4111111111111111` for
    /// `4.111111111111111e15`, unless that would be very long.
    pub(crate) fn plain(&self) -> Option<String> {
        let sign = if self.negative { "-" } else { "" };
        let length = self.digits.len() + self.power.unsigned_abs() as usize;
        if length > MAX_PLAIN_LENGTH {
            return None;
        }
        let digits = &self.digits;
        Some(if digits.is_empty() {
            "0".to_string()
        } else if self.power >= 0 {
            format!("{sign}{digits}{}", "0".repeat(self.power as usize))
        } else {
            let point = self.power.unsigned_abs() as usize;
            let padded = format!("{digits:0>point$}");
            let (whole, fraction) = padded.split_at(padded.len() - point);
            let whole = if whole.is_empty() { "0" } else { whole };
            format!("{sign}{whole}.{fraction}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_writes_the_digits_out() {
        let cases = [
            ("4.111111111111111e15", Some("4111111111111111")),
            ("41111111111111.11e2", Some("4111111111111111")),
            ("-1.5e-3", Some("-0.0015")),
            ("12.5", Some("12.5")),
            ("0", Some("0")),
            ("1e200", None),
        ];
        for (written, plain) in cases {
            let number: Number = serde_json::from_str(written).unwrap();
            let decimal = Decimal::of(&number).unwrap();
            assert_eq!(decimal.plain().as_deref(), plain, "{written}");
        }
    }
}
