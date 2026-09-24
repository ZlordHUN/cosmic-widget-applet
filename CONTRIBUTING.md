# Contributing

Thank you for contributing to COSMIC Widget.

## Before You Start

- Search existing issues before opening a new one.
- Discuss large behavioral or architectural changes in an issue first.
- Keep changes focused and avoid unrelated refactoring.
- Never commit credentials, private notification content, local cache data, or
  hardware identifiers that are not already public device IDs.

## Development Setup

Install Rust, Cargo, `just`, and the development packages required by
libcosmic and hidapi. This project targets a current COSMIC desktop session.

Build the project with:

```bash
just build-release
```

Run the applet, overlay, or settings application from source with:

```bash
cargo run --release --bin cosmic-widget-applet
cargo run --release --bin cosmic-widget-iced
cargo run --release --bin cosmic-widget-settings
```

## Source Organization

- `src/bin/` contains launchers; application startup belongs in its application module.
- `src/monitors/` contains data collection and provider integrations without UI drawing.
- `src/overlay/sections/` contains the production widget's feature views.
- `src/overlay/components/` contains reusable Iced controls and animation widgets.
- `src/applet/`, `src/settings/`, and `src/legacy/` keep each application's UI together.
- `src/runtime/` owns process locking and logging; `src/config.rs` is the shared schema.
- `assets/` contains embedded icons and fonts; `resources/` contains installed desktop files and device rules.
- `docs/` contains architecture, hardware support, and screenshots; `archive/` holds unbuilt historical snapshots.

Keep a feature's helpers and tests beside that feature. Use ordinary Rust modules
rather than importing source files through `#[path]`, and retain existing Cargo
target names and installed commands when moving code.

## Testing

Before submitting a pull request, run:

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets
```

Hardware-specific tests may require the corresponding device to be connected
and accessible through the installed udev rules. State which hardware and
desktop environment you tested in the pull request.

## Pull Requests

- Explain the problem and the behavior of the proposed solution.
- Include reproduction and verification steps.
- Add focused tests for bug fixes and new logic.
- Include before-and-after screenshots for visible interface changes.
- Update `README.md`, `docs/architecture.md`, or `docs/supported-devices.md` when the
  public behavior, architecture, or hardware support changes.

By submitting a contribution, you agree that it is licensed under the
[Mozilla Public License 2.0](LICENSE).
