#!/usr/bin/env bash
#
# Lint guard: forbid hand-rolled `a2a.{send,recv,respond}.<peer>` action
# strings in production capability-check paths.
#
# Peer-scoped a2a actions must support both forms of the peer identifier:
# the display string (`<local>@<host>`) and the base58-encoded pubkey.
# Both forms are produced by `AgentId::scoped_action_alternatives(prefix)`
# and consumed by `Server::check_capabilities_any_of`, which passes when
# any element of each alternatives group is granted. A hand-rolled
# `format!("a2a.send.{}", peer.display)` (or its `recv` / `respond`
# siblings) collapses the dual-form check back to a single form, which
# (a) silently breaks pubkey-form grants made via `covenant capabilities
# grant a2a.send.<pubkey-prefix>` and (b) re-opens the display-collision
# attack where a peer authenticating with the same `<local>@<host>` but
# a different pubkey wins a grant intended for the legitimate holder.
#
# Production code anywhere under `crates/covenantd/src/` MUST go through
# `scoped_action_alternatives` + `check_capabilities_any_of`. Test code
# is exempt because tests intentionally synthesise display-form action
# strings to exercise the matched-form branches.
#
# Scope: every `*.rs` file under `crates/covenantd/src/`. The CLI
# (`crates/covenant/src/main.rs`) and the types crate
# (`crates/covenant-types/src/lib.rs`) construct action strings as
# inputs (operator-typed grant strings, helper output) but never call
# `check_capabilities`, so a hand-rolled string there is not a regression
# of the same shape and is out of scope. The widening from `lib.rs` to
# the whole daemon source tree closes the gap a future commit could
# exploit by landing a `format!` in `http.rs` or `main.rs` (or any new
# module that joins the daemon crate later) — anything that ends up
# linked into the daemon binary risks running through a capability check
# the same way `lib.rs` does.
#
# Run from the workspace root (the directory containing `crates/`):
#
#     ./scripts/check-no-display-form-a2a.sh
#
# Exits 0 when production code is clean, 1 when a forbidden line is
# found.

set -euo pipefail

target_dir="crates/covenantd/src"

if [ ! -d "$target_dir" ]; then
    echo "check-no-display-form-a2a: target dir not found: $target_dir" >&2
    echo "  Run this script from the cargo workspace root." >&2
    exit 2
fi

pattern='format!\("a2a\.(send|recv|respond)\.'

# Collect violations across every `.rs` file under the daemon crate's
# `src/` tree. `find` walks any future submodules without a script
# update; `sort` makes the output deterministic across filesystems.
# Each file is scanned only up to its first `#[cfg(test)]` marker so
# tests stay exempt without lying about coverage.
violations=""
while IFS= read -r file; do
    test_marker_line=$(grep -n '^#\[cfg(test)\]' "$file" | head -1 | cut -d: -f1 || true)
    if [ -z "$test_marker_line" ]; then
        test_marker_line=$(($(wc -l < "$file") + 1))
    fi

    matches=$(awk -v end="$test_marker_line" 'NR < end' "$file" \
        | grep -nE "$pattern" || true)

    if [ -n "$matches" ]; then
        # Prefix each match line with the file path so a single-pass
        # CI log read points the operator at the offending file
        # without a separate header line.
        violations+=$(printf '%s\n' "$matches" | sed -E "s#^#${file}:#")
        violations+=$'\n'
    fi
done < <(find "$target_dir" -name '*.rs' | sort)

if [ -n "$violations" ]; then
    cat >&2 <<EOF
check-no-display-form-a2a: hand-rolled a2a action string in production code.

Offending lines (line numbers are within each file's production region,
before its first \`#[cfg(test)]\` marker):

$violations
Peer-scoped a2a capability checks must compose both display-form and
pubkey-b58 alternatives via:

    let alternatives = peer.scoped_action_alternatives("a2a.send");
    self.check_capabilities_any_of(scope_id, vec![alternatives], peer).await

A hand-rolled format!("a2a.send.{}", peer.display) skips the b58
alternative, breaking grants made by pubkey-prefix and re-opening the
display-collision attack.

If this match is intentional (e.g., logging a single concrete form),
either move it into a #[cfg(test)] module or refactor it to derive the
string from scoped_action_alternatives so both forms stay synchronised.
EOF
    exit 1
fi

echo "check-no-display-form-a2a: ok ($target_dir production region clean across all .rs files)"
