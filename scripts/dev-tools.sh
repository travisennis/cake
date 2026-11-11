#!/usr/bin/env sh
# dev-tools.sh — verify the pinned Cargo utilities that repository recipes and
# git hooks require.
#
# Usage: scripts/dev-tools.sh [--install] [--json]
#
# Without --install the check is read-only and suitable for a diagnostics
# command. With --install it installs every missing or mismatched pinned tool,
# keeps going when one install fails, and reports the failures together.
# --json prints the read-only result as one JSON object and cannot be combined
# with --install; each entry reports the package, pinned version, binary,
# status, installed version, and detail. The exit status still reflects whether
# every pinned tool is usable.
#
# The pin list is the single source of truth for `just setup`. Keep each pin on
# one line in exactly "cargo install <package> --version <version> --locked"
# form: scripts/dependency-sweep.py inventories these lines as the development
# tool surface. A tool passes when cargo records that exact package version and
# its executable exists in the Cargo bin directory; `cargo install --list`
# avoids PATH precedence deciding which copy is checked.

set -eu

usage() {
    cat <<'EOF'
Usage: scripts/dev-tools.sh [--install] [--json]

Verify the pinned Cargo utilities that repository recipes and git hooks
require. Without --install the check is read-only. With --install it installs
missing or mismatched pins and reports failures together. --json prints the
read-only result as JSON and cannot be combined with --install.
EOF
}

mode=check
format=text
for arg in "$@"; do
    case "$arg" in
        --install) mode=install ;;
        --json) format=json ;;
        -h|--help) usage; exit 0 ;;
        *)
            echo "usage: scripts/dev-tools.sh [--install] [--json]" >&2
            exit 2
            ;;
    esac
done

if [ "$mode" = install ] && [ "$format" = json ]; then
    echo "ERROR: --json is read-only and cannot be combined with --install" >&2
    exit 2
fi

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
        if [ "$format" = json ]; then
            printf '{"ok":false,"tools":[],"error":"cargo is unavailable; install Rust from https://rustup.rs"}\n'
        else
            echo "ERROR: cargo is unavailable; install Rust from https://rustup.rs" >&2
        fi
        exit 1
    fi
}

installed_version() { # <package>
    printf '%s\n' "$installed_list" |
        awk -v package="$1" '$1 == package && $2 ~ /^v/ { sub(/^v/, "", $2); sub(/:$/, "", $2); print $2; exit }'
}

problem_for() { # <package> <version> <binary>; sets problem_status/problem_detail/problem_installed
    problem_installed=$(installed_version "$1")
    problem_status="ok"
    problem_detail=""
    if [ -z "$problem_installed" ]; then
        problem_status="missing"
        problem_detail="not installed"
    elif [ "$problem_installed" != "$2" ]; then
        problem_status="version-mismatch"
        problem_detail="installed at $problem_installed"
    elif [ ! -x "$bin_dir/$3" ]; then
        problem_status="binary-missing"
        problem_detail="package is installed but $bin_dir/$3 is missing"
    fi
    return 0
}

json_string() { # <value>
    printf '"%s"' "$(printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr '\n' ' ')"
}

json_nullable_string() { # <value>
    if [ -n "$1" ]; then
        json_string "$1"
    else
        printf 'null'
    fi
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
        problem_for "$package" "$version" "$binary"
        if [ "$problem_status" = ok ]; then
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
json_tools=""
json_separator=""
failed=0
while IFS= read -r pin; do
    [ -n "$pin" ] || continue
    pin_fields "$pin"
    problem_for "$package" "$version" "$binary"
    installed=$problem_installed
    if [ "$problem_status" = ok ]; then
        if [ "$format" = text ]; then
            printf 'ok: %s %s (%s)\n' "$package" "$version" "$binary"
        else
            entry=$(printf '{"package":%s,"version":%s,"binary":%s,"status":%s,"installed_version":%s,"detail":null}' \
                "$(json_string "$package")" "$(json_string "$version")" "$(json_string "$binary")" \
                "$(json_string "$problem_status")" "$(json_nullable_string "$installed")")
            json_tools="${json_tools}${json_separator}${entry}"
            json_separator=,
        fi
        continue
    fi
    failed=1
    install_note=""
    case "${failed_installs:-}" in
        *"${package}: failed to install version ${version}"*)
            install_note="the install command failed; "
            ;;
    esac
    detail="${install_note}${problem_detail}"
    if [ "$format" = text ]; then
        failures="${failures}  - ${package} ${version}: ${detail}
      ${pin}
"
    else
        entry=$(printf '{"package":%s,"version":%s,"binary":%s,"status":%s,"installed_version":%s,"detail":%s}' \
            "$(json_string "$package")" "$(json_string "$version")" "$(json_string "$binary")" \
            "$(json_string "$problem_status")" "$(json_nullable_string "$installed")" \
            "$(json_nullable_string "$detail")")
        json_tools="${json_tools}${json_separator}${entry}"
        json_separator=,
    fi
done <<EOF
$pins
EOF

if [ "$format" = json ]; then
    if [ "$failed" -eq 0 ]; then
        printf '{"ok":true,"tools":[%s]}\n' "$json_tools"
        exit 0
    fi
    printf '{"ok":false,"tools":[%s]}\n' "$json_tools"
    exit 1
fi

if [ -n "$failures" ]; then
    printf 'ERROR: required development tools are missing or broken:\n%s' "$failures" >&2
    exit 1
fi

echo "All pinned development tools are installed."
