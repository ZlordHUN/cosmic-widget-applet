name := 'cosmic-widget-applet'
widget-name := 'cosmic-widget'
widget-source-name := 'cosmic-widget-iced'
settings-name := 'cosmic-widget-settings'
appid := 'com.github.zoliviragh.CosmicWidget'

rootdir := ''
prefix := '/usr/local'

# Installation paths
base-dir := absolute_path(clean(rootdir / prefix))
cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
appdata-dst := base-dir / 'share' / 'appdata' / appid + '.metainfo.xml'
bin-dst := base-dir / 'bin' / name
widget-bin-dst := base-dir / 'bin' / widget-name
settings-bin-dst := base-dir / 'bin' / settings-name
desktop-dst := base-dir / 'share' / 'applications' / appid + '.desktop'
widget-desktop-dst := base-dir / 'share' / 'applications' / appid + '.Widget.desktop'
settings-desktop-dst := base-dir / 'share' / 'applications' / appid + '.Settings.desktop'
icon-dst := base-dir / 'share' / 'icons' / 'hicolor' / 'scalable' / 'apps' / appid + '.svg'
headset-rules-dst := base-dir / 'lib' / 'udev' / 'rules.d' / '70-cosmic-widget-headsets.rules'
meteocons-license-dst := base-dir / 'share' / 'licenses' / name / 'meteocons-LICENSE'
material-icons-license-dst := base-dir / 'share' / 'licenses' / name / 'material-icons-LICENSE'

# Default recipe which runs `just build-release`
default: build-release

# Runs `cargo clean`
clean:
    cargo clean

# Removes vendored dependencies
clean-vendor:
    rm -rf .cargo vendor vendor.tar

# `cargo clean` and removes vendored dependencies
clean-dist: clean clean-vendor

# Compiles with debug profile
build-debug *args:
    cargo build {{args}}

# Compiles with release profile
build-release *args: (build-debug '--release' args)

# Compiles release profile with vendored dependencies
build-vendored *args: vendor-extract (build-release '--frozen --offline' args)

# Runs a clippy check
check *args:
    cargo clippy --all-features {{args}} -- -W clippy::pedantic

# Runs a clippy check with JSON message format
check-json: (check '--message-format=json')

# Run the application for testing purposes
run *args:
    env RUST_BACKTRACE=full cargo run --release --bin cosmic-widget-applet {{args}}

# Run the widget for testing purposes
run-widget *args:
    env RUST_BACKTRACE=full cargo run --release --bin cosmic-widget-iced {{args}}

# Run the settings app for testing purposes
run-settings *args:
    env RUST_BACKTRACE=full cargo run --release --bin cosmic-widget-settings {{args}}

# Run the production libcosmic/Iced widget
run-iced-widget *args:
    env RUST_BACKTRACE=full cargo run --release --bin cosmic-widget-iced {{args}}

# Check all packaged resources before building or copying installation files.
[private]
_check-install-resources:
    #!/usr/bin/env bash
    set -euo pipefail
    for source in resources/app.desktop resources/widget.desktop resources/settings.desktop resources/app.metainfo.xml resources/icon.svg resources/70-cosmic-widget-headsets.rules assets/weather/meteocons/LICENSE assets/icons/material-design/LICENSE; do
        if [[ ! -f "$source" ]]; then
            printf 'Missing installation file: %s\n' "$source" >&2
            exit 1
        fi
    done

# Refresh all installed binaries without ever running Cargo as root.
[private]
_prepare-install: _check-install-resources
    #!/usr/bin/env bash
    set -euo pipefail
    target_dir={{ quote(cargo-target-dir) }}
    build=(build --locked --release --target-dir "$target_dir" --bin {{ quote(name) }} --bin {{ quote(widget-source-name) }} --bin {{ quote(settings-name) }})
    if [[ "$EUID" == 0 ]]; then
        build_user="${SUDO_USER:-}"
        if [[ -z "$build_user" && "${PKEXEC_UID:-}" =~ ^[0-9]+$ ]]; then
            build_user="$(id -nu -- "$PKEXEC_UID" 2>/dev/null || true)"
        fi
        if [[ -z "$build_user" ]] || ! build_uid="$(id -u -- "$build_user" 2>/dev/null)" || [[ "$build_uid" == 0 ]]; then
            printf 'Cannot identify a non-root build user. Run sudo just install from your normal account, or build as that user and run just install-prebuilt.\n' >&2
            exit 1
        fi
        printf 'Refreshing release binaries as %s...\n' "$build_user"
        sudo -H -u "$build_user" -- sh -c 'PATH="$HOME/.cargo/bin:$PATH"; export PATH; exec cargo "$@"' sh "${build[@]}"
    else
        cargo "${build[@]}"
    fi

# Validate every binary before copying any installation files.
[private]
_check-install-binaries:
    #!/usr/bin/env bash
    set -euo pipefail
    target_dir={{ quote(cargo-target-dir) }}
    for binary in {{ quote(name) }} {{ quote(widget-source-name) }} {{ quote(settings-name) }}; do
        if [[ ! -x "$target_dir/release/$binary" ]]; then
            printf 'Missing release binary: %s. Run just build-release first.\n' "$target_dir/release/$binary" >&2
            exit 1
        fi
    done

# Build current sources as the invoking user, then install all production files.
install: _prepare-install install-prebuilt

# Install already-built release files (for packaging or a direct root session).
install-prebuilt: _check-install-resources _check-install-binaries
    install -Dm0755 {{ quote(cargo-target-dir / 'release' / name) }} {{ quote(bin-dst) }}
    install -Dm0755 {{ quote(cargo-target-dir / 'release' / widget-source-name) }} {{ quote(widget-bin-dst) }}
    install -Dm0755 {{ quote(cargo-target-dir / 'release' / settings-name) }} {{ quote(settings-bin-dst) }}
    install -Dm0644 resources/app.desktop {{ quote(desktop-dst) }}
    install -Dm0644 resources/widget.desktop {{ quote(widget-desktop-dst) }}
    install -Dm0644 resources/settings.desktop {{ quote(settings-desktop-dst) }}
    install -Dm0644 resources/app.metainfo.xml {{ quote(appdata-dst) }}
    install -Dm0644 resources/icon.svg {{ quote(icon-dst) }}
    install -Dm0644 resources/70-cosmic-widget-headsets.rules {{ quote(headset-rules-dst) }}
    install -Dm0644 assets/weather/meteocons/LICENSE {{ quote(meteocons-license-dst) }}
    install -Dm0644 assets/icons/material-design/LICENSE {{ quote(material-icons-license-dst) }}

# Uninstalls installed files
uninstall:
    rm {{bin-dst}} {{widget-bin-dst}} {{settings-bin-dst}} {{desktop-dst}} {{widget-desktop-dst}} {{settings-desktop-dst}} {{icon-dst}} {{headset-rules-dst}} {{meteocons-license-dst}} {{material-icons-license-dst}}

# Vendor dependencies locally
vendor:
    mkdir -p .cargo
    cargo vendor --sync Cargo.toml | head -n -1 > .cargo/config.toml
    echo 'directory = "vendor"' >> .cargo/config.toml
    echo >> .cargo/config.toml
    rm -rf .cargo vendor

# Extracts vendored dependencies
vendor-extract:
    rm -rf vendor
    tar pxf vendor.tar

# Bump cargo version, create git commit, and create tag
tag version:
    find -type f -name Cargo.toml -exec sed -i '0,/^version/s/^version.*/version = "{{version}}"/' '{}' \; -exec git add '{}' \;
    cargo check
    cargo clean
    git add Cargo.lock
    git commit -m 'release: {{version}}'
    git tag -a {{version}} -m ''
