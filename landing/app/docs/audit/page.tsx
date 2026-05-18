import Link from "next/link";
import { buildDocsMetadata } from "../_meta";

export const metadata = buildDocsMetadata("audit", "Audit log", 'JSONL audit log: event variants, integrity sidecar, schema, and how to read it.');

export default function AuditPage() {
  return (
    <>
      <h1>Audit log</h1>
      <p>
        Every state-changing surface in Covenant emits an{" "}
        <code>AuditEvent</code> to the append-only log at{" "}
        <code>$COVENANT_HOME/audit/events.jsonl</code>. The log is the
        ground truth — operators read it directly,{" "}
        <code>covenant verify</code> cross-checks it against the other
        state files, <code>covenant audit verify</code> checks the local
        hash-chain sidecar, and the <code>covenant audit recent</code>{" "}
        route reads from the same file.
      </p>

      <h2>Event envelope</h2>
      <pre>
        <code>{`AuditEvent {
  id:           uuid,                  // unique per event
  timestamp_ms: u64,                   // unix milliseconds
  issuer:       AgentId,               // the daemon's local identity
  kind:         AuditKind              // tagged variant — see below
}`}</code>
      </pre>

      <h2>Variants</h2>

      <h3>
        <code>IntentDispatched</code>
      </h3>
      <pre>
        <code>{`{
  "type":           "intent_dispatched",
  "intent_id":      "uuid",
  "intent_text":    "…",
  "matched_agent":  "research" | null,
  "result_hash_hex": "…",
  "status":         "ok"
}`}</code>
      </pre>

      <h3>
        <code>IntentIgnored</code>
      </h3>
      <pre>
        <code>{`{
  "type":            "intent_ignored",
  "intent_id":       "uuid",
  "intent_text":     "…",
  "matched_pattern": "**/*.pem"
}`}</code>
      </pre>

      <h3>
        <code>CapabilityCheck</code>
      </h3>
      <pre>
        <code>{`{
  "type":              "capability_check",
  "agent_id":          "research" | "tool:echo",
  "required_actions":  ["tool.web_search"],
  "missing_actions":   [],
  "passed":            true
}`}</code>
      </pre>

      <h3>
        <code>CapabilityGranted</code>
      </h3>
      <pre>
        <code>{`{
  "type":               "capability_granted",
  "subject_display":    "user@local",
  "action":             "tool.web_search",
  "granted_by_display": "user@local",
  "signature_b58":      "4qXP…8tF1"
}`}</code>
      </pre>

      <h3>
        <code>CapabilityGrantRejected</code>
      </h3>
      <pre>
        <code>{`{
  "type":            "capability_grant_rejected",
  "subject_display": "user@local",
  "action":          "memory.write",
  "reason":          "scope schema: missing tier"
}`}</code>
      </pre>
      <p>
        Emitted when the daemon refuses a <code>CreateCapability</code>{" "}
        request at scope validation — for example, a{" "}
        <code>memory.write</code> grant whose scope object does not
        match the action&apos;s schema. Distinct from{" "}
        <code>CapabilityRevokeRejected</code> (rejection on the revoke
        path, keyed on signature) and from <code>CapabilityCheck</code>{" "}
        (a dispatch-time check against an already-issued capability):
        no capability is issued here, and <code>reason</code> carries
        the validator message the caller saw. The requesting peer is
        the issuer of the row.
      </p>

      <h3>
        <code>CapabilityScopeRejected</code>
      </h3>
      <pre>
        <code>{`{
  "type":     "capability_scope_rejected",
  "agent_id": "memory:write",
  "action":   "memory.write",
  "reason":   "tier 'long_term' outside scope"
}`}</code>
      </pre>
      <p>
        Emitted at dispatch time when the caller holds a capability for{" "}
        <code>action</code> but the request falls outside its{" "}
        <code>scope</code> — for example, an{" "}
        <code>audit.purge</code> call whose <code>before_ms</code>{" "}
        predates the scope window, or a <code>memory.write</code> at a
        tier the grant did not authorise. <code>agent_id</code> is a
        synthetic id naming the runtime surface that performed the
        check (<code>memory:write</code>, <code>tool:web_search</code>,{" "}
        <code>audit:purge</code>); <code>action</code> is the
        capability action string; <code>reason</code> is the
        scope-mismatch message returned to the caller. Distinct from{" "}
        <code>CapabilityCheck</code> with{" "}
        <code>passed: false</code>, which fires when a required action
        is missing entirely from the issuer&apos;s capability set —
        scope rejection assumes the action <em>is</em> present and the
        rejection is on its bound scope.
      </p>

      <h3>
        <code>CapabilityRevokeRejected</code>
      </h3>
      <pre>
        <code>{`{
  "type":          "capability_revoke_rejected",
  "signature_b58": "4qXP…8tF1",
  "reason":        "issuer mismatch"
}`}</code>
      </pre>
      <p>
        Emitted when the daemon refuses a capability revocation
        request — for example, an issuer that does not own the
        signature or a payload whose signature does not verify.
        Successful revocations are <em>not</em> audit events;
        they write tombstone rows to the capability store
        instead. The audit log only records the rejected attempt
        so unauthorized revocation requests stay visible to
        operators.
      </p>

      <h3>
        <code>AuthenticationFailed</code>
      </h3>
      <pre>
        <code>{`{
  "type":      "authentication_failed",
  "transport": "http",
  "reason":    "missing Authorization header"
}`}</code>
      </pre>
      <p>
        Emitted on every rejected authentication attempt — a bad first
        frame on the IPC socket, a missing or malformed{" "}
        <code>Authorization</code> header on HTTP, or a token the
        registry does not resolve. <code>transport</code> is{" "}
        <code>&quot;ipc&quot;</code> or <code>&quot;http&quot;</code>;{" "}
        <code>reason</code> is the same short message the caller saw.
        Because the caller failed to authenticate, no peer identity is
        bound to the row: the issuer is the daemon&apos;s own
        <code>AgentId</code>, which keeps probe-attribution events
        visible on the operator&apos;s <code>/audit</code> feed
        independent of the cross-peer audit-feed isolation filter.
        Distinct from <code>OperatorTokenRotationRejected</code> and{" "}
        <code>OperatorPeerRevokeRejected</code>, which fire after a peer
        authenticates and then fails an authorization gate.
      </p>

      <h3>
        <code>PeerRevoked</code>
      </h3>
      <pre>
        <code>{`{
  "type":            "peer_revoked",
  "peer_display":    "guest@local",
  "peer_pubkey_b58": "9hYz…Lk6w",
  "token_prefix":    "4qXP6t"
}`}</code>
      </pre>
      <p>
        Emitted when the operator successfully revokes a peer registry
        entry via <code>RevokePeer</code>. The issuer is the operator
        (peer-event audience: the recording path asserts that the
        issuer&apos;s pubkey matches the acting peer&apos;s);{" "}
        <code>peer_display</code> and <code>peer_pubkey_b58</code>{" "}
        describe the <em>revoked</em> peer, not the operator;{" "}
        <code>peer_pubkey_b58</code> is the unforgeable identifier
        (display is wire-supplied). <code>token_prefix</code> is the
        same 6-char base58 redaction that{" "}
        <code>OperatorTokenRotated</code> records — full token bytes
        never enter the audit log, and the prefix lets an operator
        confirm a revoked entry matches the durable on-disk peer
        registry. Distinct from <code>OperatorPeerRevokeRejected</code>{" "}
        (daemon-issuer probe by a non-operator) and{" "}
        <code>PeerSelfRevokeBlocked</code> (operator-issuer fat-finger
        on their own bootstrap token).
      </p>

      <h3>
        <code>PeerSelfRevokeBlocked</code>
      </h3>
      <pre>
        <code>{`{
  "type":            "peer_self_revoke_blocked",
  "peer_display":    "user@local",
  "peer_pubkey_b58": "9hYz…Lk6w",
  "token_prefix":    "4qXP6t"
}`}</code>
      </pre>
      <p>
        Emitted when the operator&apos;s <code>RevokePeer</code>{" "}
        request would have revoked their own bootstrap token but{" "}
        <code>force</code> was <code>false</code>. The daemon returns{" "}
        <code>RevokeOutcome::SelfRevokeForbidden</code> and the
        registry is unchanged. The operator IS both the issuer and the
        audience here, so the row surfaces in their own{" "}
        <code>/audit</code> feed for self-fat-finger triage —
        deliberately distinct from <code>OperatorPeerRevokeRejected</code>,
        which records a non-operator&apos;s probe under the
        daemon-as-issuer audience so the operator can see the probe on
        their feed without exposing it to the rejected peer.{" "}
        <code>peer_pubkey_b58</code> is the unforgeable operator
        identity (the <code>display</code> is wire-supplied) and{" "}
        <code>token_prefix</code> is the same 6-char base58 redaction{" "}
        <code>PeerRevoked</code> and <code>OperatorTokenRotated</code>{" "}
        record.
      </p>

      <h3>
        <code>OperatorTokenRotated</code>
      </h3>
      <pre>
        <code>{`{
  "type":             "operator_token_rotated",
  "peer_display":     "user@local",
  "old_token_prefix": "4qXP6t",
  "new_token_prefix": "8wPN3z"
}`}</code>
      </pre>
      <p>
        Emitted when the operator rotates their bootstrap token via{" "}
        <code>RotateOperatorToken</code>. The issuer is the operator
        (peer-event audience — the recording path asserts the
        issuer&apos;s pubkey matches the acting peer&apos;s). Full
        token bytes never enter the audit log: both prefixes are the
        same 6-char base58 redaction that <code>PeerRevoked</code>{" "}
        records and that <code>PeerToken::Debug</code> formats, so an
        operator can correlate a rotation row with the on-disk token
        file&apos;s first chars without ever exposing the secret.{" "}
        <code>new_token_prefix</code> is load-bearing: it lets an
        operator confirm, after any rotation they did or did not
        initiate, that the file on disk came from this row rather than
        a silent compromise. Distinct from{" "}
        <code>OperatorTokenRotationRejected</code>, the daemon-issuer
        probe row recorded when a non-operator peer authenticates but
        fails the identity-pubkey equality gate.
      </p>

      <h3>
        <code>BudgetExhausted</code>
      </h3>
      <pre>
        <code>{`{
  "type":             "budget_exhausted",
  "agent_display":    "research@local",
  "intent_id":        "uuid",
  "intent_text":      "…",
  "requested":        1024,
  "tokens_remaining": 256,
  "refill_eta_ms":    600000
}`}</code>
      </pre>
      <p>
        Emitted by the daemon when the matched agent&apos;s token-bucket
        ledger refuses a debit. The same row doubles as the resume
        queue:{" "}
        <code>covenant intents resume {`<intent-id>`}</code>{" "}
        re-dispatches from this event, so the six fields above are
        load-bearing.
      </p>

      <h3>
        <code>BudgetPreempted</code>
      </h3>
      <pre>
        <code>{`{
  "type":          "budget_preempted",
  "agent_display": "research@local",
  "intent_id":     "uuid",
  "reason":        "projected cpu overshoot at 28.4s/30s",
  "signal_sent":   "SIGTERM",
  "exit_code":     null
}`}</code>
      </pre>
      <p>
        Emitted when the budget-hard-preempt path successfully
        terminates a still-running over-budget subprocess. Distinct
        from <code>BudgetExhausted</code> (a post-completion debit
        rejection on a finished run): preempt actively kills a
        running process. <code>signal_sent</code> classifies the
        path the daemon took — <code>&quot;SIGTERM&quot;</code> for
        the cooperative-grace attempt, <code>&quot;SIGKILL&quot;</code>{" "}
        when the subprocess outlived the grace window, and{" "}
        <code>&quot;none&quot;</code> when the subprocess exited
        naturally inside the grace window before any signal was
        needed. <code>exit_code</code> is <code>null</code> for
        signal-terminated processes (the daemon observed termination
        via <code>child.wait()</code> only); the field surfaces as
        JSON null on the wire so the schema stays stable across
        cooperative-exit and forced-kill rows. The five fields are
        load-bearing for post-mortems that classify cooperative vs.
        forced terminations.
      </p>

      <h3>
        <code>BudgetPreemptFailed</code>
      </h3>
      <pre>
        <code>{`{
  "type":          "budget_preempt_failed",
  "agent_display": "research@local",
  "intent_id":     "uuid",
  "reason":        "kill(SIGTERM) returned ESRCH",
  "errno":         3
}`}</code>
      </pre>
      <p>
        Emitted when the budget-hard-preempt path attempted to signal
        the subprocess but the signal-send syscall returned an error.
        The <code>errno</code> field is operationally load-bearing
        for triage: <code>3</code> (<code>ESRCH</code>) is benign — the
        subprocess exited before the daemon could signal it, which the
        daemon may also surface as a <code>BudgetPreempted</code> row
        with <code>signal_sent: &quot;none&quot;</code> on a different
        code path. <code>1</code> (<code>EPERM</code>) is a security
        incident: the daemon lacks signal-send permission for that
        pid, and an operator should investigate before the bucket fills
        again. The four fields are load-bearing for incident triage and
        for any future tooling that classifies the
        cooperative-race-vs-privilege-regression axis.
      </p>

      <h3>
        <code>BudgetUnseeded</code>
      </h3>
      <pre>
        <code>{`{
  "type":          "budget_unseeded",
  "agent_display": "research@local",
  "intent_id":     "uuid",
  "requested":     1024
}`}</code>
      </pre>
      <p>
        Emitted when <code>dispatch_intent</code> falls into the
        NoCapacity fail-open arm: the manifest opted in to budget
        enforcement (<code>budget_credits_per_hour &gt; 0</code>) but
        no bucket was seeded for the agent — the operator forgot to
        call <code>register_agent_budgets</code>, or a hot-reload
        added the manifest without re-seeding. v0 logs the row and
        passes the dispatch. The variant is deliberately distinct
        from <code>BudgetExhausted</code> so <code>/audit</code>{" "}
        consumers can filter operator-misconfig from policy-rejection
        without special-casing sentinel values: a collapsed schema
        would force consumers to disambiguate by inspecting
        <code>tokens_remaining</code>, which only exists on the
        exhausted row.
      </p>

      <h2>Properties</h2>
      <ul>
        <li>
          <strong>Append-only during normal writes.</strong> The file is
          opened for append on event record. Operator-driven retention
          purge rewrites the retained rows and the sidecar together.
        </li>
        <li>
          <strong>Locally chained.</strong> The daemon writes{" "}
          <code>$COVENANT_HOME/audit/events.chain.jsonl</code> with a
          SHA-256 hash chain over retained event rows.
        </li>
        <li>
          <strong>One event per line.</strong> Compatible with{" "}
          <code>tail -F</code>, <code>jq</code>, and other JSONL-aware
          tooling.
        </li>
        <li>
          <strong>Deterministic schema.</strong> Each variant
          serialises with a stable <code>kind</code> tag plus its
          payload. Adding new variants is a backward-compatible
          schema change.
        </li>
        <li>
          <strong>Cross-checked.</strong>{" "}
          <code>covenant verify</code> runs four audits over a
          rolling window:
          <ul>
            <li>
              memory ↔ audit — every memory record has a matching{" "}
              <code>IntentDispatched</code>.
            </li>
            <li>
              memory parent references — every parent id resolves in
              the memory store.
            </li>
            <li>
              capability ↔ audit — every granted capability has a
              matching <code>CapabilityGranted</code>.
            </li>
            <li>
              memory ↔ receipts — memory writes and settlement
              receipts pair by <code>memory_record_id</code>, with
              legacy count fallback.
            </li>
          </ul>
        </li>
      </ul>

      <h2>Reading the log</h2>

      <h3>Last few events</h3>
      <pre>
        <code>{`covenant audit recent --limit 5 --json
# Or via HTTP:
curl -s '127.0.0.1:8421/audit/recent?limit=5&since_ms=1714938000000' | jq`}</code>
      </pre>

      <h3>Verify local chain</h3>
      <pre>
        <code>{`covenant audit verify
curl -s 127.0.0.1:8421/audit/verify \\
  -H "Authorization: Bearer $COVENANT_OPERATOR_TOKEN" | jq`}</code>
      </pre>

      <h3>Filter for capability checks that failed</h3>
      <pre>
        <code>{`tail -F ~/.covenant/audit/events.jsonl \\
  | jq -c 'select(.kind.type == "capability_check" and .kind.passed == false)'`}</code>
      </pre>

      <h3>Find every dispatch for a specific agent</h3>
      <pre>
        <code>{`jq -c 'select(.kind.type == "intent_dispatched"
              and .kind.matched_agent == "research")' \\
  ~/.covenant/audit/events.jsonl`}</code>
      </pre>

      <h2>Trust model</h2>
      <p>
        The audit log is local. A user with write access to{" "}
        <code>$COVENANT_HOME</code> can rewrite history. The local
        hash-chain detects retained-row edits and sidecar mismatch after
        anchoring, and <code>covenant verify</code> surfaces
        cross-reference drift. This is not public signing or immutable
        storage.
      </p>
      <p>
        Deployments where the operator is not the sole writer to the host
        should either sign individual events or stream the log to an
        append-only system with the appropriate trust model. Both
        approaches integrate against the existing <code>AuditLog</code>{" "}
        trait.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/audit-integrity">Audit integrity</Link> — local
          hash-chain verification and its limits.
        </li>
        <li>
          <Link href="/capabilities">Capability tokens</Link> —
          where grants and checks originate.
        </li>
        <li>
          <Link href="/cli">CLI</Link> — <code>verify</code> and
          its drift-check rules.
        </li>
        <li>
          <Link href="/security">Security model</Link> — what the
          local-trust assumption costs you.
        </li>
      </ul>
    </>
  );
}
