//! Symbol processing and character name lookup
//!
//! Note: Phonetic alphabet is defined in `state/phonetics.rs` as the `PHONETICS` map.

use std::collections::HashMap;

/// Replace symbols in text with their names
pub fn process_symbols(text: &str, symbols: &HashMap<u32, String>) -> String {
    let mut result = String::new();

    for ch in text.chars() {
        if let Some(name) = symbols.get(&(ch as u32)) {
            result.push(' ');
            result.push_str(name);
            result.push(' ');
        } else {
            result.push(ch);
        }
    }

    result
}

/// Replace repeated characters with count + character
/// e.g., "====" becomes "4 equals"
pub fn condense_repeated_chars(
    text: &str,
    chars_to_condense: &str,
    symbols: &HashMap<u32, String>,
) -> String {
    if chars_to_condense.is_empty() || text.is_empty() {
        return text.to_string();
    }

    // Rust's regex crate doesn't support backreferences, so we do this manually
    let condense_set: std::collections::HashSet<char> = chars_to_condense.chars().collect();

    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if condense_set.contains(&ch) {
            // Count consecutive occurrences of this character
            let mut count = 1;
            while chars.peek() == Some(&ch) {
                chars.next();
                count += 1;
            }

            if count > 1 {
                // Get symbol name or use the character itself
                let char_name = symbols
                    .get(&(ch as u32))
                    .map(|s| s.as_str())
                    .unwrap_or("");

                if char_name.is_empty() {
                    result.push_str(&format!("{} {}", count, ch));
                } else {
                    result.push_str(&format!("{} {}", count, char_name));
                }
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_condense_repeated() {
        let symbols = HashMap::new();

        // Basic test
        let result = condense_repeated_chars("====", "=", &symbols);
        assert_eq!(result, "4 =");

        // Multiple groups
        let result = condense_repeated_chars("===---", "-=", &symbols);
        assert_eq!(result, "3 =3 -");

        // With symbol names
        let mut symbols_with_names = HashMap::new();
        symbols_with_names.insert('=' as u32, "equals".to_string());
        let result = condense_repeated_chars("====", "=", &symbols_with_names);
        assert_eq!(result, "4 equals");

        // Single char not condensed
        let result = condense_repeated_chars("=", "=", &symbols);
        assert_eq!(result, "=");

        // Mixed content
        let result = condense_repeated_chars("hello===world", "=", &symbols);
        assert_eq!(result, "hello3 =world");
    }
}
