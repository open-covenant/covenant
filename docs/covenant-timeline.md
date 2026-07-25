# Covenant Timeline Integration

Status: design boundary; adapter not yet implemented

Covenant Timeline is an incubating standalone protocol for replaying evidenced
checkpoints in long-running software and agent work. Covenant is its first
reference adopter.

## Ownership boundary

The standalone Timeline project owns:

- contract, event, evidence, decision, command, and receipt schemas;
- deterministic reduction and replay;
- conformance fixtures;
- portable SDK behavior.

Covenant owns only the adapter between those objects and its implemented
runtime surfaces.

The Covenant repository must not copy or fork the Timeline reducer. Once a
preview package exists, the adapter will consume a pinned release or commit.

## Mapping

| Covenant surface | Timeline object |
| --- | --- |
| Commit-scoped provenance envelope | Evidence |
| Audit event or integrity report | Event or evidence |
| Policy and review outcome | Evidence |
| Timeline checkpoint evaluation | Decision |
| Capability request | Command |
| Runtime or settlement result | Receipt |

## Integration sequence

1. Publish a standalone pre-alpha Timeline package and fixture version.
2. Define a one-way, deterministic mapping from Covenant provenance records to
   evidence objects.
3. Export a Covenant engineering run as a portable Timeline event stream.
4. Translate accepted Timeline commands into explicit Covenant capability
   requests.
5. Return Covenant outcomes as receipt events.
6. Verify the exported run with Covenant stopped.

## Safety boundary

- Timeline decisions do not grant Covenant capabilities.
- Covenant re-evaluates authorization, expiry, scope, and operator policy.
- Replay never invokes the Covenant adapter.
- Missing or unverified provenance remains visible.
- Exported records contain no secrets or private evidence payloads by default.

The integration is not complete until a real multi-checkpoint Covenant build can
be exported and independently replayed.
