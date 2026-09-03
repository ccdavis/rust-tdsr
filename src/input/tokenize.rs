//! Split a raw stdin read into individual key sequences.
//!
//! A single `read()` on the terminal can return more than one keystroke:
//! key auto-repeat (holding Alt+o), fast typing, or a paste all arrive as one
//! chunk. The keymap matches whole sequences, so a chunk like `ESC o ESC o`
//! must be split into two `ESC o` keys before dispatch, otherwise nothing
//! matches and the raw bytes leak through to the shell.

/// Bracketed-paste start/end markers (xterm `CSI 200 ~` / `CSI 201 ~`).
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Split `input` into key sequences, in order.
///
/// Recognized shapes:
/// - `ESC [ ... final` — CSI (arrow keys, Delete, function keys); params
///   `0x30..=0x3F`, intermediates `0x20..=0x2F`, final `0x40..=0x7E`.
/// - `ESC O x` — SS3 (application-mode arrows and keypad).
/// - `ESC <char>` — Alt/Meta + key; the char may be multi-byte UTF-8.
/// - a lone `ESC` at the end of the chunk, or `ESC ESC`, yields a bare ESC.
/// - a bracketed paste (`ESC [ 200 ~` ... `ESC [ 201 ~`) is one token so its
///   body is never interpreted as keys; an unterminated paste runs to the
///   end of the chunk.
/// - `ESC ]` ... `BEL`/`ESC \` (OSC) and `ESC P` ... `ESC \` (DCS) are one
///   token each: these are the terminal answering an application's query.
/// - anything else is one UTF-8 scalar (or one byte if malformed).
pub fn split_keys(input: &[u8]) -> Vec<&[u8]> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < input.len() {
        let len = key_len(&input[i..]);
        keys.push(&input[i..i + len]);
        i += len;
    }
    keys
}

/// Length of the key sequence at the start of `buf` (`buf` is non-empty).
fn key_len(buf: &[u8]) -> usize {
    if buf[0] != 0x1b {
        return utf8_len(buf);
    }
    if buf.len() == 1 {
        return 1; // lone ESC
    }
    match buf[1] {
        0x1b => 1, // ESC ESC: emit a bare ESC, the next ESC starts a new key
        b'[' => {
            if buf.starts_with(PASTE_START) {
                return paste_len(buf);
            }
            // CSI: skip parameter and intermediate bytes, stop after the final.
            let mut i = 2;
            while i < buf.len() {
                let b = buf[i];
                if (0x40..=0x7e).contains(&b) {
                    return i + 1;
                }
                if !(0x20..=0x3f).contains(&b) {
                    // Malformed: treat "ESC [" as its own token.
                    return 2;
                }
                i += 1;
            }
            buf.len() // truncated sequence; keep what we have
        }
        b'O' => {
            // SS3: ESC O <one byte>
            if buf.len() >= 3 {
                3
            } else {
                buf.len()
            }
        }
        // OSC / DCS strings run to BEL or ST (ESC \)
        b']' | b'P' => string_len(buf),
        _ => 1 + utf8_len(&buf[1..]), // Alt + (possibly multi-byte) key
    }
}

/// Length of an OSC/DCS string starting at `buf[0]`, through its terminator
/// (BEL or ESC \), or to the end of the chunk if unterminated.
fn string_len(buf: &[u8]) -> usize {
    let mut i = 2;
    while i < buf.len() {
        match buf[i] {
            0x07 => return i + 1,
            0x1b if buf.get(i + 1) == Some(&b'\\') => return i + 2,
            _ => i += 1,
        }
    }
    buf.len()
}

/// Whether `key` is something the terminal sent on its own rather than a
/// keystroke: a reply to an application's query (cursor position report,
/// device attributes, DSR, DECRPM, OSC and DCS replies) or a focus event.
/// These are forwarded to the shell untouched and must not silence speech.
pub fn is_terminal_response(key: &[u8]) -> bool {
    match key {
        [0x1b, b']', ..] | [0x1b, b'P', ..] => true,
        [0x1b, b'[', rest @ ..] if !rest.is_empty() => {
            let body = &rest[..rest.len() - 1];
            match rest[rest.len() - 1] {
                // Cursor position report: CSI row ; col R
                b'R' => {
                    body.contains(&b';') && body.iter().all(|b| b.is_ascii_digit() || *b == b';')
                }
                // Device attributes: CSI ? ... c / CSI > ... c
                b'c' => matches!(body.first(), Some(b'?') | Some(b'>')),
                // Device status report reply: CSI 0 n
                b'n' => !body.is_empty() && body.iter().all(u8::is_ascii_digit),
                // DECRPM mode report: CSI ? mode ; value $ y
                b'y' => body.contains(&b'$'),
                // Focus in / focus out
                b'I' | b'O' => body.is_empty(),
                _ => false,
            }
        }
        _ => false,
    }
}

/// Length of a bracketed paste starting at `buf[0]`, through its end marker
/// (or to the end of the chunk if unterminated).
fn paste_len(buf: &[u8]) -> usize {
    let body = &buf[PASTE_START.len()..];
    match body.windows(PASTE_END.len()).position(|w| w == PASTE_END) {
        Some(pos) => PASTE_START.len() + pos + PASTE_END.len(),
        None => buf.len(),
    }
}

/// Length of the UTF-8 scalar at the start of `buf`, clamped to the buffer.
/// Malformed input yields one byte so nothing is ever skipped.
fn utf8_len(buf: &[u8]) -> usize {
    let want = match buf[0] {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    };
    let len = want.min(buf.len());
    // Only take continuation bytes; stop early on anything else.
    let mut n = 1;
    while n < len && (buf[n] & 0xc0) == 0x80 {
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(input: &[u8]) -> Vec<Vec<u8>> {
        split_keys(input).into_iter().map(|k| k.to_vec()).collect()
    }

    #[test]
    fn single_keys_pass_through_unchanged() {
        assert_eq!(split(b"a"), vec![b"a".to_vec()]);
        assert_eq!(split(b"\x1bu"), vec![b"\x1bu".to_vec()]);
        assert_eq!(split(b"\x1b[A"), vec![b"\x1b[A".to_vec()]);
        assert_eq!(split(b"\x1bOA"), vec![b"\x1bOA".to_vec()]);
        assert_eq!(split(b"\x1b[3~"), vec![b"\x1b[3~".to_vec()]);
        assert_eq!(split(b"\x7f"), vec![b"\x7f".to_vec()]);
        assert_eq!(split(b""), Vec::<Vec<u8>>::new());
    }

    #[test]
    fn lone_escape_is_forwarded() {
        assert_eq!(split(b"\x1b"), vec![b"\x1b".to_vec()]);
        assert_eq!(split(b"\x1b\x1b"), vec![b"\x1b".to_vec(), b"\x1b".to_vec()]);
    }

    #[test]
    fn key_repeat_splits_into_repeated_alt_keys() {
        assert_eq!(
            split(b"\x1bo\x1bo\x1bo"),
            vec![b"\x1bo".to_vec(), b"\x1bo".to_vec(), b"\x1bo".to_vec()]
        );
    }

    #[test]
    fn typed_char_before_alt_key() {
        assert_eq!(split(b"a\x1bu"), vec![b"a".to_vec(), b"\x1bu".to_vec()]);
        assert_eq!(split(b"\x1bux"), vec![b"\x1bu".to_vec(), b"x".to_vec()]);
    }

    #[test]
    fn mixed_csi_and_text() {
        assert_eq!(
            split(b"\x1b[A\x1b[Bq"),
            vec![b"\x1b[A".to_vec(), b"\x1b[B".to_vec(), b"q".to_vec()]
        );
        // Modifier params: Shift+Up
        assert_eq!(split(b"\x1b[1;2A"), vec![b"\x1b[1;2A".to_vec()]);
    }

    #[test]
    fn utf8_scalars_stay_whole() {
        let s = "é😀".as_bytes();
        assert_eq!(
            split(s),
            vec!["é".as_bytes().to_vec(), "😀".as_bytes().to_vec()]
        );
        // Alt + multibyte char
        let alt = [b"\x1b".as_slice(), "é".as_bytes()].concat();
        assert_eq!(split(&alt), vec![alt.clone()]);
    }

    #[test]
    fn malformed_utf8_never_skips_bytes() {
        assert_eq!(split(b"\xff\xfe"), vec![b"\xff".to_vec(), b"\xfe".to_vec()]);
        // Lead byte without continuation
        assert_eq!(split(b"\xc3a"), vec![b"\xc3".to_vec(), b"a".to_vec()]);
    }

    #[test]
    fn bracketed_paste_is_one_token() {
        let paste = b"\x1b[200~ls \x1bu\n\x1b[201~";
        assert_eq!(split(paste), vec![paste.to_vec()]);
        let with_tail = b"\x1b[200~x\x1b[201~\x1bi";
        assert_eq!(
            split(with_tail),
            vec![b"\x1b[200~x\x1b[201~".to_vec(), b"\x1bi".to_vec()]
        );
        // Unterminated paste runs to end of chunk
        let open = b"\x1b[200~hello";
        assert_eq!(split(open), vec![open.to_vec()]);
    }

    #[test]
    fn truncated_csi_is_kept_whole() {
        assert_eq!(split(b"\x1b["), vec![b"\x1b[".to_vec()]);
        assert_eq!(split(b"\x1b[1;"), vec![b"\x1b[1;".to_vec()]);
        assert_eq!(split(b"\x1bO"), vec![b"\x1bO".to_vec()]);
    }

    #[test]
    fn osc_and_dcs_replies_are_single_tokens() {
        let osc = b"\x1b]11;rgb:0000/0000/0000\x1b\\";
        assert_eq!(split(osc), vec![osc.to_vec()]);
        let osc_bel = b"\x1b]11;rgb:0/0/0\x07\x1bu";
        assert_eq!(
            split(osc_bel),
            vec![b"\x1b]11;rgb:0/0/0\x07".to_vec(), b"\x1bu".to_vec()]
        );
        let dcs = b"\x1bP1+r5463\x1b\\";
        assert_eq!(split(dcs), vec![dcs.to_vec()]);
    }

    #[test]
    fn terminal_responses_are_recognised() {
        assert!(is_terminal_response(b"\x1b[24;80R"));
        assert!(is_terminal_response(b"\x1b[?1;2c"));
        assert!(is_terminal_response(b"\x1b[>0;95;0c"));
        assert!(is_terminal_response(b"\x1b[0n"));
        assert!(is_terminal_response(b"\x1b[?2026;2$y"));
        assert!(is_terminal_response(b"\x1b]11;rgb:0/0/0\x07"));
        assert!(is_terminal_response(b"\x1bP1+r5463\x1b\\"));
        assert!(is_terminal_response(b"\x1b[I"));
        assert!(is_terminal_response(b"\x1b[O"));
        // keys are not responses
        assert!(!is_terminal_response(b"\x1b[A"));
        assert!(!is_terminal_response(b"\x1bu"));
        assert!(!is_terminal_response(b"c"));
        assert!(!is_terminal_response(b"\x1b[3~"));
        assert!(!is_terminal_response(b"\x1bOA"));
    }
}
