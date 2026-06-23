# covenant-sns-signer

The write sidecar for the SNS profile. `covenantd` spawns it per write, pipes a
`SignerRequest` JSON on stdin, and reads a `SignerResponse` JSON on stdout. The
parent-domain keypair lives only here, never in the daemon.

It builds writes with the Bonfida SDK (`@bonfida/spl-name-service`), simulates
every transaction first, then submits.

## Protocol

Request (stdin):

```json
{"kind":"register_subdomain","parent":"opencovenant.sol","subdomain":"foundation","owner":"<base58 wallet>"}
{"kind":"set_record","domain":"opencovenant.sol","record":"url","value":"https://opencovenant.org"}
```

Response (stdout):

```json
{"signature":"<sig>","domain":"foundation.opencovenant.sol","cluster":"mainnet-beta"}
```

Non-zero exit + stderr message on any failure (simulation, submit, bad input).

## Subdomain binding

`register_subdomain` creates `<subdomain>.<parent>` under the parent key, then
transfers it to `owner` when `owner` differs from the parent key. (The Bonfida
helper's reverse-registry step assumes the new owner is the signing parent
owner, so direct creation for a third party fails; create-then-transfer is the
working path.)

## set_record scope

Writes records on domains the signer owns: the parent, or a subdomain before it
is transferred. Records on an already-transferred subdomain are written by that
subdomain's owner, not this sidecar.

## Env

| Var | Required | Meaning |
| --- | --- | --- |
| `COVENANT_SNS_RPC_URL` | yes | Solana RPC for simulate + submit. |
| `COVENANT_SNS_KEYPAIR` | yes | Path to the parent-domain owner keypair JSON. |
| `COVENANT_SNS_SUBDOMAIN_SPACE` | no | Bytes allocated per subdomain (default 1000). |

`covenantd` forwards these (plus `PATH`/`HOME` so the `node` shebang resolves)
and sets `signer_binary` to this `index.mjs`. Run standalone for testing:

```sh
echo '{"kind":"register_subdomain","parent":"opencovenant.sol","subdomain":"foundation","owner":"<wallet>"}' \
  | COVENANT_SNS_RPC_URL=<rpc> COVENANT_SNS_KEYPAIR=<path> ./index.mjs
```
