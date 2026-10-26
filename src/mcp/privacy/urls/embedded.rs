//! One match can hold several addresses run together. Each later address
//! starts an authority that is not a field value (not right after `=`).

use super::super::patterns::AUTHORITY;

const FIELD_CHARACTERS: [char; 2] = ['=', '&'];

#[derive(Default)]
struct Delimiters {
    query: bool,
    fragment: bool,
    fragment_query: bool,
}

impl Delimiters {
    /// Whether this `?` or `#` is one of a URL's three delimiters, consuming it.
    fn consume(&mut self, character: char, last_structural: char) -> bool {
        match character {
            '#' => !std::mem::replace(&mut self.fragment, true),
            '?' if self.fragment && last_structural == '=' => false,
            '?' if self.fragment => !std::mem::replace(&mut self.fragment_query, true),
            '?' => !std::mem::replace(&mut self.query, true),
            _ => false,
        }
    }
}

pub(super) fn address_starts(text: &str) -> Vec<usize> {
    if !text.contains("://") {
        return Vec::new();
    }
    let authorities: Vec<usize> = AUTHORITY
        .find_iter(text)
        .map(|found| found.start())
        .collect();
    let mut starts = Vec::new();
    let mut delimiters = Delimiters::default();
    let mut pending = authorities.iter().peekable();
    let mut in_fields = false;
    let mut last_structural = '\0';
    for (offset, character) in text.char_indices() {
        let Some(&&authority) = pending.peek() else {
            break;
        };
        if authority == offset {
            if offset > 0 && last_structural != '=' {
                starts.push(offset);
            }
            pending.next();
        }
        if delimiters.consume(character, last_structural) {
            last_structural = character;
            in_fields = true;
        } else if in_fields && FIELD_CHARACTERS.contains(&character) {
            last_structural = character;
        }
    }
    starts
}
