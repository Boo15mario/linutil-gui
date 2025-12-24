# Repository Guidelines

## Project Structure & Module Organization
- `tui/` holds the GTK4 desktop UI crate (primary executable).
- `core/` contains shared logic, tab data, and scripts referenced by the UI.
- `xtask/` contains developer automation tasks.
- `docs/` and `man/` contain documentation and manual pages.
- `nix/` and `flake.nix` support Nix-based development.
- Shell entrypoints live in `start.sh` and `startdev.sh`.

## Build, Test, and Development Commands
- `cargo build` compiles the workspace (default members: `tui`, `core`).
- `cargo run` builds and runs the GTK app from `tui/`.
- `cargo test` runs Rust tests (currently minimal in this repo).
- `./start.sh` runs the latest release binary from GitHub.
- `./startdev.sh` runs the latest pre-release binary from GitHub.

## Coding Style & Naming Conventions
- Rust edition: 2021 (see workspace settings in `Cargo.toml`).
- Formatting: use `rustfmt` with `rustfmt.toml`.
- Naming: `snake_case` for functions/vars, `PascalCase` for types, `SCREAMING_SNAKE_CASE` for constants.
- Prefer small, focused functions and avoid holding UI borrows across GTK signal callbacks.

## Testing Guidelines
- No formal test suite is enforced; `cargo test` is still expected to pass when tests exist.
- Shell validation exists under `core/tabs/applications-setup/test-script-access.sh`.
- If you add tests, keep names descriptive and colocate with the crate under test.

## Commit & Pull Request Guidelines
- Use short, imperative commit messages (e.g., “Fix GTK selection crash”).
- Keep PRs focused; include a clear description of behavior changes and any manual test steps.
- For UI changes, note relevant accessibility or keyboard behavior.

## Accessibility & UX Notes
- The GTK UI should remain usable with screen readers like Orca.
- Provide accessible labels/description properties for interactive widgets.
- Ensure keyboard-only navigation works (focus traversal + shortcuts).
