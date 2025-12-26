# Code Review Report

## Scope
- Reviewed Rust sources under `src/` plus `README.md` for configuration semantics.
- Focused on bugs, behavior gaps, and redundancy.

## Findings

### 1) Terminal output does not wrap or scroll at edges (High)
References: `src/terminal/performer.rs:46`, `src/terminal/performer.rs:86`, `src/terminal/performer.rs:92`.
- `print` clamps the cursor to the last column with `min(cols - 1)` and does not wrap to the next line when text reaches the right edge.
- `\n` only increments `y` and clamps to the last row, so output never scrolls when the terminal fills.
- Result: long lines and multiline output overwrite the last column/row instead of wrapping/scrolling, so the screen buffer diverges from what a terminal would show. This can break review cursor navigation and speech accuracy for normal shell output.

### 2) `scroll_up` shrinks the screen buffer (High)
References: `src/terminal/screen.rs:145`, `src/terminal/screen.rs:156`.
- `scroll_up` removes the top line, then only inserts a blank line if `bottom < self.buffer.len() as u16`.
- When `bottom` is the last row (the default), the remove reduces `len` by 1, the condition fails, and no new row is inserted.
- Result: the buffer permanently loses rows after scrolling, which will eventually misalign cursor math and line access.

### 3) `cursor_delay` unit mismatch between UI/config/docs (Medium)
References: `src/input/config_handler.rs:224`, `src/state/config.rs:308`, `src/input/default_handler.rs:265`, `README.md:91`.
- The config UI says it takes milliseconds and divides by 1000 before writing (`set_cursor_delay`).
- Runtime reads the value as seconds and passes it to `Duration::from_secs_f32`.
- The README documents the value in milliseconds, so a user who edits `~/.tdsr.cfg` to `cursor_delay = 300` will get a 300-second delay instead of 300 ms.

### 4) `key_echo` and `line_pause` settings are persisted but unused (Medium)
References: `src/input/config_handler.rs:112`, `src/state/config.rs:252`, `src/input/buffer_handler.rs:53`, `src/terminal/performer.rs:92`.
- Both options can be toggled and are stored in config.
- `key_echo` is not applied anywhere (only a TODO exists in the buffer handler).
- `line_pause` is only mentioned in a comment; there is no runtime logic that changes speech output based on it.
- Result: users can toggle these settings, but they have no effect.

### 5) Redundant symbol processing logic (Low)
References: `src/symbols.rs:7`, `src/state/mod.rs:282`, `src/state/mod.rs:377`.
- There is a full symbol-processing implementation in `src/symbols.rs`, but the runtime uses separate logic in `State::process_symbols_in_text` and `State::replace_duplicate_characters`.
- This duplication is easy to let drift and makes it unclear which implementation is authoritative.

### 6) Minor redundant keymap construction (Low)
References: `src/main.rs:137`, `src/main.rs:139`.
- `create_default_keymap()` is called twice: once to build the keymap and again just to log `len()`.
- This adds an extra allocation and can be avoided by reusing the existing `keymap`.
