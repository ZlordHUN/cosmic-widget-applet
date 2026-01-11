name := 'cosmic-widget-applet'
widget-name := 'cosmic-widget'
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
    env RUST_BACKTRACE=full cargo run --release {{args}}

# Run the widget for testing purposes
run-widget *args:
    env RUST_BACKTRACE=full cargo run --release --bin cosmic-monitor-widget {{args}}

# Run the settings app for testing purposes
run-settings *args:
    env RUST_BACKTRACE=full cargo run --release --bin cosmic-monitor-settings {{args}}

# Installs files
install:
    install -Dm0755 {{ cargo-target-dir / 'release' / name }} {{bin-dst}}
    install -Dm0755 {{ cargo-target-dir / 'release' / widget-name }} {{widget-bin-dst}}
    install -Dm0755 {{ cargo-target-dir / 'release' / settings-name }} {{settings-bin-dst}}
    install -Dm0644 resources/app.desktop {{desktop-dst}}
    install -Dm0644 resources/widget.desktop {{widget-desktop-dst}}
    install -Dm0644 resources/settings.desktop {{settings-desktop-dst}}
    install -Dm0644 resources/app.metainfo.xml {{appdata-dst}}
    install -Dm0644 resources/icon.svg {{icon-dst}}

# Uninstalls installed files
uninstall:
    rm {{bin-dst}} {{widget-bin-dst}} {{settings-bin-dst}} {{desktop-dst}} {{widget-desktop-dst}} {{settings-desktop-dst}} {{icon-dst}}

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

