# gitlawb-push-demo — verifiable agent commits on gitlawb

A Covenant-governed agent makes a real, RFC 9421-signed `git-receive-pack` push
to a [gitlawb](https://github.com/Gitlawb/node) node, then binds a verifiable
`covenant/exec/v1` attestation to the ref certificate the node issues for that
push. It uses only gitlawb's existing signed API — nothing changes node-side.

## What it proves

1. `did:key` identity + RFC 9421 request signing interoperate with `gitlawb-node`.
2. A Covenant agent can push a real commit over a signed `git-receive-pack`.
3. The node's signed ref certificate wraps into a `covenant/exec/v1` attestation
   that binds the agent's identity, the capability it held, its sandbox digest,
   and its audit root to that exact commit.

So a gitlawb ref-update carries cryptographic proof of *who* produced the commit
and *how* — not just that someone with a key pushed.

## Run

Point it at any reachable gitlawb node (default `http://127.0.0.1:7545`):

```sh
# in a Gitlawb/node checkout, start a node however you normally do, e.g.:
docker compose up

# then, here:
GITLAWB_NODE_URL=http://127.0.0.1:7545 cargo run
```

## Expected output

```text
[1] registered agent + created repo 'covenant-commit-…'
[2] built commit …  (296 byte packfile)
[3] POST git-receive-pack (signed)      -> unpack ok: true, ref ok: true
[4] node issued ref cert …
[5] covenant/exec/v1 over the node cert -> fully_verified: true
```

## How it works

- `GitlawbClient::receive_pack` (in the `covenant-gitlawb` crate) signs the
  request — including the exact URL path, which carries the `did:key` owner and
  so contains `:` — and POSTs a standard smart-HTTP `receive-pack` body
  (pkt-line ref command + packfile).
- The packfile is built with the local `git` binary; the agent's key is the
  authorization (the node derives repo ownership and the pusher DID from the
  signature).
- The node issues a ref certificate for the push. `AttestedRefUpdateCert` wraps
  that cert and a `covenant/exec/v1` attestation is signed over its hash; the
  attestation verifies through the `gitlawb-attest` registry.

See [`../gitlawb-live-demo`](../gitlawb-live-demo) for the register/repo flow on
its own, and [`../gitlawb-attest-demo`](../gitlawb-attest-demo) for the offline
attestation roundtrip.
