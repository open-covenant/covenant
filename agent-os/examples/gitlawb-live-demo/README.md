# gitlawb-live-demo — Covenant identity on a live gitlawb node

A Covenant agent registers, creates a repo, and attaches a `covenant/exec/v1`
attestation — all against a running [gitlawb](https://github.com/Gitlawb/node)
node, proving `did:key` + RFC 9421 signing interoperate with `gitlawb-node`.

For the full path including a real signed `git-receive-pack` push, see
[`../gitlawb-push-demo`](../gitlawb-push-demo).

## Run

Point it at any reachable gitlawb node (default `http://127.0.0.1:7545`):

```sh
GITLAWB_NODE_URL=http://127.0.0.1:7545 cargo run
```

## Steps

1. `GET /health`
2. `POST /api/register` (RFC 9421 signed) — the node accepts the agent's signature
3. `POST /api/v1/repos` (signed) — create a repo owned by the agent DID
4. `GET /api/v1/repos/{owner}/{repo}` — read it back
5. bind + verify a `covenant/exec/v1` attestation for `refs/heads/main`
