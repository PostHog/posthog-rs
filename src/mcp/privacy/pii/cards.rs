//! Card numbers: a digit run is redacted only where whole groups of it are
//! 13 to 19 digits long and pass the Luhn check.

use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::mcp::contract::REDACTED;

static CANDIDATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)[0-9](?:[ ./-]?[0-9]){12,}(?-u:\b)").expect("card candidate pattern")
});

static DIGIT_GROUP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("[0-9]+").expect("digit group pattern"));

struct DigitGroup<'a> {
    digits: &'a str,
    start: usize,
    end: usize,
}

pub(super) fn redact_cards(text: &str) -> String {
    CANDIDATE
        .replace_all(text, |captures: &Captures<'_>| {
            redact_in_candidate(&captures[0])
        })
        .into_owned()
}

fn redact_in_candidate(candidate: &str) -> String {
    let groups: Vec<DigitGroup<'_>> = DIGIT_GROUP
        .find_iter(candidate)
        .map(|found| DigitGroup {
            digits: found.as_str(),
            start: found.start(),
            end: found.end(),
        })
        .collect();
    let mut output = String::new();
    let mut cursor = 0;
    let mut first = 0;
    while first < groups.len() {
        match longest_card_from(&groups, first) {
            Some(last) => {
                output += &candidate[cursor..groups[first].start];
                output += REDACTED;
                cursor = groups[last].end;
                first = last + 1;
            }
            None => first += 1,
        }
    }
    output + &candidate[cursor..]
}

fn longest_card_from(groups: &[DigitGroup<'_>], first: usize) -> Option<usize> {
    let mut digits = String::new();
    let mut longest = None;
    for (last, group) in groups.iter().enumerate().skip(first) {
        digits += group.digits;
        if digits.len() > 19 {
            break;
        }
        if digits.len() >= 13 && passes_luhn(&digits) {
            longest = Some(last);
        }
    }
    longest
}

fn passes_luhn(digits: &str) -> bool {
    let mut sum = 0;
    for (position, byte) in digits.bytes().rev().enumerate() {
        let digit = u32::from(byte - b'0');
        let doubled = if position % 2 == 1 { digit * 2 } else { digit };
        sum += if doubled > 9 { doubled - 9 } else { doubled };
    }
    sum % 10 == 0
}
