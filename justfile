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

# Check installation inputs and build missing binaries without running Cargo as root.
[private]
_prepare-install:
    #!/usr/bin/env bash
    set -euo pipefail
    for source in resources/app.desktop resources/widget.desktop resources/settings.desktop resources/app.metainfo.xml resources/icon.svg resources/70-cosmic-widget-headsets.rules assets/weather/meteocons/LICENSE assets/icons/material-design/LICENSE; do
        if [[ ! -f "$source" ]]; then
            printf 'Missing installation file: %s\n' "$source" >&2
            exit 1
        fi
    done
    target_dir={{ quote(cargo-target-dir) }}
    binaries=({{ quote(name) }} {{ quote(widget-source-name) }} {{ quote(settings-name) }})
    missing=false
    for binary in "${binaries[@]}"; do
        if [[ ! -x "$target_dir/release/$binary" ]]; then
            printf 'Missing release binary: %s\n' "$target_dir/release/$binary" >&2
            missing=true
        fi
    done
    if [[ "$missing" == false ]]; then
        exit 0
    fi
    build=(build --locked --release --target-dir "$target_dir")
    for binary in "${binaries[@]}"; do
        build+=(--bin "$binary")
    done
    if [[ "$EUID" == 0 ]]; then
        if [[ -z "${SUDO_USER:-}" || "$SUDO_USER" == root ]]; then
            printf 'Run just build-release as your normal user, then run the installation again.\n' >&2
            exit 1
        fi
        printf 'Building missing release binaries as %s...\n' "$SUDO_USER"
        sudo -H -u "$SUDO_USER" -- sh -c 'PATH="$HOME/.cargo/bin:$PATH"; export PATH; exec cargo "$@"' sh "${build[@]}"
    else
        cargo "${build[@]}"
    fi
    for binary in "${binaries[@]}"; do
        if [[ ! -x "$target_dir/release/$binary" ]]; then
            printf 'Build did not produce %s\n' "$target_dir/release/$binary" >&2
            exit 1
        fi
    done

# Installs prebuilt files, building any missing release binaries first.
install: _prepare-install
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
