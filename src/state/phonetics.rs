//! Phonetic alphabet for character spelling
//!
//! When user asks for phonetic spelling (alt+comma twice), we use the
//! NATO phonetic alphabet to clarify which letter was spoken.

use std::collections::HashMap;
use once_cell::sync::Lazy;

/// NATO phonetic alphabet
///
/// Maps each letter to its phonetic word (a -> "alpha", b -> "bravo", etc.)
/// Screen reader uses this for unambiguous character identification
pub static PHONETICS: Lazy<HashMap<char, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert('a', "alpha");
    m.insert('b', "bravo");
    m.insert('c', "charlie");
    m.insert('d', "delta");
    m.insert('e', "echo");
    m.insert('f', "foxtrot");
    m.insert('g', "golf");
    m.insert('h', "hotel");
    m.insert('i', "india");
    m.insert('j', "juliet");
    m.insert('k', "kilo");
    m.insert('l', "lima");
    m.insert('m', "mike");
    m.insert('n', "november");
    m.insert('o', "oscar");
    m.insert('p', "papa");
    m.insert('q', "quebec");
    m.insert('r', "romeo");
    m.insert('s', "sierra");
    m.insert('t', "tango");
    m.insert('u', "uniform");
    m.insert('v', "victor");
    m.insert('w', "whiskey");
    m.insert('x', "x ray");
    m.insert('y', "yankee");
    m.insert('z', "zulu");
    m
});
