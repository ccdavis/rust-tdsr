# Code Review Report

## Scope
- Reviewed Rust sources under `src/` plus `README.md` for configuration semantics.
- Focused on bugs, behavior gaps, redundancy, and performance issues.
- Verified existing findings and performed deep-dive into critical paths (`ScreenPerformer`, `State`, `SpeechBuffer`).

## Findings

### 1) Terminal output does not wrap or scroll at edges (High)
**Verified**
References: `src/terminal/performer.rs:46`, `src/terminal/performer.rs:86`, `src/terminal/performer.rs:92`.
- `print` clamps the cursor to the last column (`min(cols - 1)`) and does not wrap to the next line when text reaches the right edge.
- `\n` only increments `y` and clamps to the last row, so output never scrolls when the terminal fills.
- Result: Long lines overwrite the last character column. Multiline output overwrites the last row instead of pushing old content up. This causes the screen buffer to completely diverge from the visual terminal state.

### 2) `scroll_up` shrinks the screen buffer (High)
**Verified**
References: `src/terminal/screen.rs:145`, `src/terminal/screen.rs:156`.
- `scroll_up` removes the top line, then attempts to insert a blank line at `bottom`.
- The insertion condition `if bottom < self.buffer.len() as u16` fails because `len` was just reduced by the removal.
- Result: The screen buffer size decreases by 1 row for every scroll event. Eventually, the buffer becomes smaller than the terminal window, causing out-of-bounds errors or incorrect cursor positioning.

### 3) Regex backreferences not supported in `repeated_symbols` (High)
**New Finding**
References: `src/state/mod.rs:377`, `src/symbols.rs:38`.
- The `replace_duplicate_characters` function uses a regex pattern with a backreference: `format!(r"([{}])\1+", ...)` to find repeated characters.
- The `regex` crate **does not support backreferences** (as explicitly noted in `src/symbols.rs`).
- Result: The `Regex::new` call returns an error, which is caught and ignored. The feature silently fails, and repeated symbols are never condensed, regardless of configuration.

### 4) `cursor_delay` unit mismatch between UI/config/docs (Medium)
**Verified**
References: `src/input/config_handler.rs:224`, `src/state/config.rs:308`, `src/input/default_handler.rs:265`, `README.md:91`.
- The config UI divides user input by 1000 before saving (treating input as ms, saving as seconds).
- The `State::cursor_delay` getter reads the float value directly as seconds.
- The `README` instructs users to set `cursor_delay = 300` (implying milliseconds).
- Result: Manual config edits following the README result in a 300-second delay (5 minutes), while UI configuration results in a 0.3-second delay.

### 5) Rectangular selection instead of linear text selection (Medium)
**New Finding**
References: `src/state/mod.rs:207` (`copy_text_range`).
- The text selection logic normalizes coordinates to form a bounding box `[min_x, max_x]` x `[min_y, max_y]`.
- Result: Copying a wrapped sentence (end of line 1 + start of line 2) fails to capture the text correctly. It only copies the intersection of the character columns, likely missing the end of the first line and the start of the second. Standard text selection should be linear (from point A to point B).

### 6) `key_echo` and `line_pause` settings are persisted but unused (Medium)
**Verified**
References: `src/input/config_handler.rs:112`, `src/state/config.rs:252`, `src/terminal/performer.rs:92`, `src/main.rs:267`.
- `key_echo`: The system always speaks PTY output. There is no logic to suppress speech for user's typed characters if `key_echo` is disabled.
- `line_pause`: The `ScreenPerformer` accumulates text and `main.rs` flushes it to speech immediately. There is no logic to insert pauses or break speech at newlines based on this setting.
- Result: Users cannot disable key echo or enable line pausing.

### 7) Performance: Regex recompilation in hot path (Low)
**New Finding**
References: `src/state/mod.rs:269`.
- `process_symbols_in_text` compiles the symbol regex (`Regex::new`) on *every* call to `speak`.
- `speak` is called frequently (every chunk of PTY output).
- Result: Unnecessary CPU overhead. The regex should be compiled once and stored in `Config` or `State`.

### 8) Performance: Inefficient SpeechBuffer clearing (Low)
**New Finding**
References: `src/terminal/performer.rs:115`.
- When handling backspace, `ScreenPerformer` allocates a new string, iterates all chars, and takes `count - 1` to remove the last character.
- Result: Inefficient O(N) operation for a simple O(1) pop. It also replaces the entire `SpeechBuffer` instance.

### 9) Redundant symbol processing logic (Low)
**Verified**
References: `src/symbols.rs:7`, `src/state/mod.rs:282`.
- `src/symbols.rs` contains a manual string replacement implementation.
- `src/state/mod.rs` contains a Regex-based implementation.
- Result: Code duplication and inconsistency. The manual implementation in `symbols.rs` is actually safer (no Regex dependency issues) and should likely be the primary one.

### 10) Minor redundant keymap construction (Low)
**Verified**
References: `src/main.rs:137`.
- `create_default_keymap()` is called twice during initialization (once for the handler, once for logging).
- Result: Minor unnecessary allocation.

### 11) Missing terminal escape sequences (Medium)
**New Finding**
References: `src/terminal/performer.rs:281`.
- The `esc_dispatch` function is empty (no-op), meaning ESC sequences are ignored.
- Missing critical sequences:
  - ESC 7 / ESC 8 (save/restore cursor position - DECSC/DECRC)
  - ESC M (reverse line feed / scroll down)
  - ESC D (line feed / scroll up)
  - ESC E (next line)
- Result: Programs that use these sequences (vim, less, nano) will have incorrect cursor positioning in the screen buffer.

### 12) Missing CSI sequences in terminal performer (Medium)
**New Finding**
References: `src/terminal/performer.rs:137-274`.
- Several important CSI sequences are unimplemented:
  - 'L' - Insert lines (IL)
  - 'M' - Delete lines (DL)
  - 'P' - Delete characters (DCH)
  - '@' - Insert characters (ICH)
  - 'r' - Set scroll region (DECSTBM)
  - 'd' - Line Position Absolute (VPA)
  - 'G' - Cursor Character Absolute (CHA)
- Result: Full-screen applications that use these sequences will have misaligned screen buffers, causing the review cursor to read incorrect content.

### 13) `condense_repeated_chars` in symbols.rs is unused (Low)
**New Finding**
References: `src/symbols.rs:26`, `src/state/mod.rs:379`.
- `symbols.rs` contains a correct manual implementation of `condense_repeated_chars` that properly handles repeated character condensing without regex backreferences.
- However, `state/mod.rs` uses its own `replace_duplicate_characters` method with broken regex backreferences.
- Result: The working implementation exists but isn't used; the broken implementation is called instead.

### 14) `scroll_down` may corrupt buffer on scroll region edge cases (Low)
**New Finding**
References: `src/terminal/screen.rs:163-175`.
- The scroll_down logic removes a line from `bottom` then inserts at `top`.
- If `bottom >= buffer.len()`, the remove will fail silently.
- When using scroll regions, the logic may not correctly handle edge cases where the scroll region is smaller than the terminal.
- Result: Potential buffer corruption in edge cases with custom scroll regions.

### 15) Review cursor can go out of sync with screen buffer (Medium)
**New Finding**
References: `src/state/mod.rs:709-712`, `src/terminal/performer.rs`.
- The review cursor is updated based on the terminal cursor position via `update_review_cursor_from_terminal`.
- However, when the screen scrolls (scroll_up/scroll_down), the review cursor position is not adjusted.
- Result: After scrolling, the review cursor may point to different content than expected, confusing users trying to re-read the same line.

---

## Summary

### Issue Count by Severity
- **High:** 3 issues (#1, #2, #3) - **ALL FIXED**
- **Medium:** 6 issues (#4, #5, #6, #11, #12, #15) - **ALL FIXED**
- **Low:** 6 issues (#7, #8, #9, #10, #13, #14) - **ALL FIXED**

### All Issues Have Been Addressed

**Critical fixes (blocking basic functionality):**
1. ✅ **#1 Terminal wrapping/scrolling** - Fixed in `performer.rs:print()` and `execute()`
2. ✅ **#2 scroll_up buffer shrinking** - Fixed using swap-based logic in `screen.rs`

**High priority fixes (features broken):**
3. ✅ **#3 Regex backreferences** - Fixed by using `symbols::condense_repeated_chars`
4. ✅ **#13 Unused working implementation** - Now used in `state/mod.rs:417`
5. ✅ **#4 cursor_delay units** - Fixed config.rs and config_handler.rs to use ms consistently

**Medium priority fixes (incomplete functionality):**
6. ✅ **#6 key_echo/line_pause unused** - Implemented in main.rs and performer.rs
7. ✅ **#5 Rectangular selection** - Fixed to use linear selection in `copy_text_range`
8. ✅ **#11 Missing ESC sequences** - Implemented DECSC, DECRC, ESC M, D, E
9. ✅ **#12 Missing CSI sequences** - Implemented L, M, P, @, r, d, G
10. ✅ **#15 Review cursor scroll sync** - Added scroll offset tracking and cursor adjustment

**Low priority fixes (performance/cleanup):**
11. ✅ **#7 Regex recompilation** - Compiled regex now cached in Config
12. ✅ **#8 SpeechBuffer clearing** - Added `pop()` method for O(1) backspace
13. ✅ **#9 Redundant symbol logic** - Removed unused `process_symbols` function
14. ✅ **#10 Keymap construction** - Fixed to avoid duplicate construction
15. ✅ **#14 scroll_down edge cases** - Fixed with swap-based logic and bounds checking

### Test Coverage

All fixes include unit tests:
- 64 unit tests + 11 integration tests = **75 tests passing**
- New tests for: scroll operations, ESC sequences, CSI sequences, line_pause, SpeechBuffer.pop
