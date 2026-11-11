#!/usr/bin/env sh
# dev-tools.sh — verify the pinned Cargo utilities that repository recipes and
# git hooks require.
#
# Usage: scripts/dev-tools.sh [--install]
#
# Without --install the check is read-only and suitable for a diagnostics
# command. With --install it installs every missing or mismatched pinned tool,
# keeps going when one install fails, and reports the failures together.
#
# The pin list is the single source of truth for `just setup`. Keep each pin on
# one line in exactly "cargo install <package> --version <version> --locked"
# form: scripts/dependency-sweep.py inventories these lines as the development
# tool surface. A tool passes when cargo records that exact package version and
# its executable exists in the Cargo bin directory; `cargo install --list`
# avoids PATH precedence deciding which copy is checked.

set -eu

mode=check
case "${1:-}" in
    "") ;;
    --install) mode=install ;;
    -h|--help)
        sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *)
        echo "usage: scripts/dev-tools.sh [--install]" >&2
        exit 2
        ;;
esac

cargo_home=${CARGO_HOME:-"$HOME/.cargo"}
bin_dir="$cargo_home/bin"

pins='cargo install cargo-edit --version 0.13.13 --locked
cargo install cargo-deny --version 0.20.2 --locked
cargo install cargo-insta --version 1.48.0 --locked
cargo install cargo-llvm-cov --version 0.9.0 --locked
cargo install cargo-crap --version 0.2.2 --locked
cargo install panache --version 3.0.0 --locked
cargo install prek --version 0.4.13 --locked
cargo install cocogitto --version 7.0.0 --locked
'

binary_for() { # <package>
    case "$1" in
        cargo-edit) echo cargo-add ;;
        cocogitto) echo cog ;;
        *) echo "$1" ;;
    esac
}

read_installed() {
    if ! installed_list=$(cargo install --list 2>/dev/null); then
        echo "ERROR: cargo is unavailable; install Rust from https://rustup.rs" >&2
        exit 1
    fi
}

installed_version() { # <package>
    printf '%s\n' "$installed_list" |
        awk -v package="$1" '$1 == package && $2 ~ /^v/ { sub(/^v/, "", $2); sub(/:$/, "", $2); print $2; exit }'
}

problem_for() { # <package> <version> <binary>; empty when the tool is usable
    installed=$(installed_version "$1")
    if [ -z "$installed" ]; then
        echo "not installed"
    elif [ "$installed" != "$2" ]; then
        echo "installed at $installed"
    elif [ ! -x "$bin_dir/$3" ]; then
        echo "package is installed but $bin_dir/$3 is missing"
    fi
    return 0
}

# The pin format is fixed and validated by the dependency sweep, so positional
# words are the package and version. Word splitting is intentional here.
# shellcheck disable=SC2086
pin_fields() { # <pin line>; sets package, version, binary
    set -- $1
    if [ "$#" -ne 6 ]; then
        echo "ERROR: malformed dev-tool pin: $1" >&2
        exit 1
    fi
    package=$3
    version=$5
    binary=$(binary_for "$package")
}

read_installed

if [ "$mode" = install ]; then
    while IFS= read -r pin; do
        [ -n "$pin" ] || continue
        pin_fields "$pin"
        problem=$(problem_for "$package" "$version" "$binary")
        if [ -z "$problem" ]; then
            continue
        fi
        printf 'Installing %s %s...\n' "$package" "$version"
        if ! cargo install "$package" --version "$version" --locked --quiet; then
            failed_installs="${failed_installs:-}${package}: failed to install version ${version}
"
        fi
    done <<EOF
$pins
EOF
    read_installed
fi

failures=""
while IFS= read -r pin; do
    [ -n "$pin" ] || continue
    pin_fields "$pin"
    problem=$(problem_for "$package" "$version" "$binary")
    if [ -z "$problem" ]; then
        printf 'ok: %s %s (%s)\n' "$package" "$version" "$binary"
        continue
    fi
    install_note=""
    case "${failed_installs:-}" in
        *"${package}: failed to install version ${version}"*)
            install_note="the install command failed; "
            ;;
    esac
    failures="${failures}  - ${package} ${version}: ${install_note}${problem}
      ${pin}
"
done <<EOF
$pins
EOF

if [ -n "$failures" ]; then
    printf 'ERROR: required development tools are missing or broken:\n%s' "$failures" >&2
    exit 1
fi

echo "All pinned development tools are installed."
