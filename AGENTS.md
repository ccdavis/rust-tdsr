# Repository Guidelines

## Project Structure and Module Organization
TDSR is a Rust workspace with source under `src/` and supporting docs at the root.
Key areas:
- `src/terminal/`, `src/speech/`, `src/input/`, `src/state/`, `src/review/`, `src/plugins/` for core modules.
- `tests/` for integration tests.
- `plugins/` and `examples/` for plugin documentation and samples.
- `tdsr.cfg.dist` for the default config template. Build output goes to `target/`.

## Build, Test, and Development Commands
- `cargo build --release` builds the binary at `target/release/tdsr`.
- `cargo run -- --debug` runs locally with debug logging.
- `cargo test` runs all automated tests.
- `cargo test speech` runs speech-specific tests.
- `cargo fmt` formats code (rustfmt).
- `cargo clippy` runs lint checks.
- `./build.sh` runs the repo build script; `--no-test` skips tests, `--clean` resets build outputs.

## Coding Style and Naming Conventions
- Use standard Rust formatting (4-space indent, rustfmt defaults).
- Prefer idiomatic Rust naming: `snake_case` for modules and functions, `CamelCase` for types.
- Keep module boundaries aligned with the directory layout (for example, speech backends in `src/speech/`).

## Testing Guidelines
- Automated tests are expected to pass even without TTS available; still run `cargo test` before changes.
- For manual speech verification, follow `TESTING.md` and ensure the platform TTS backend works.
- Use descriptive `#[test]` names in `snake_case` and place integration tests in `tests/`.

## Commit and Pull Request Guidelines
- No commit history exists yet, so there is no established convention. Use short, imperative subjects (for example, "Add pulseaudio fallback") and keep scope clear.
- Pull requests should include a brief summary, test commands run, and any platform notes (Linux/macOS/WSL).
- Attach logs for speech issues (`tdsr.log`) instead of screenshots unless UI output is relevant.

## Configuration and Runtime Tips
- User config lives at `~/.tdsr.cfg`; `tdsr.cfg.dist` is the starting template.
- Plugin setup and examples are described in `plugins/README.md`.
