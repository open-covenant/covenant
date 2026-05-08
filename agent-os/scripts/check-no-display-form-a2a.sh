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
# Production code in `crates/covenantd/src/lib.rs` MUST go through
# `scoped_action_alternatives` + `check_capabilities_any_of`. Test code
# is exempt because tests intentionally synthesise display-form action
# strings to exercise the matched-form branches.
#
# Scope: only the daemon's library entry point, where the capability
# check actually runs. The CLI (`crates/covenant/src/main.rs`) and the
# types crate (`crates/covenant-types/src/lib.rs`) construct action
# strings as inputs (operator-typed grant strings, helper output) but
# never call `check_capabilities`, so a hand-rolled string there is not
# a regression of the same shape.
#
# Run from the workspace root (the directory containing `crates/`):
#
#     ./scripts/check-no-display-form-a2a.sh
#
# Exits 0 when production code is clean, 1 when a forbidden line is
# found.

set -euo pipefail

target="crates/covenantd/src/lib.rs"

if [ ! -f "$target" ]; then
    echo "check-no-display-form-a2a: target file not found: $target" >&2
    echo "  Run this script from the cargo workspace root." >&2
    exit 2
fi

# Production region = lines before the first `#[cfg(test)]` marker.
# `awk` exits at that line; if the file has no such marker we walk the
# whole file. A regression that lands a hand-rolled action string in a
# new file (other than `lib.rs`) is out of scope for this guard — the
# scope is the file where `check_capabilities` actually runs.
test_marker_line=$(grep -n '^#\[cfg(test)\]' "$target" | head -1 | cut -d: -f1 || true)
if [ -z "$test_marker_line" ]; then
    test_marker_line=$(($(wc -l < "$target") + 1))
fi

pattern='format!\("a2a\.(send|recv|respond)\.'

matches=$(awk -v end="$test_marker_line" 'NR < end' "$target" \
    | grep -nE "$pattern" || true)

if [ -n "$matches" ]; then
    cat >&2 <<EOF
check-no-display-form-a2a: hand-rolled a2a action string in production code.

Offending lines in $target (line numbers are within the production
region, before line $test_marker_line):

$matches

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

echo "check-no-display-form-a2a: ok ($target production region clean)"
