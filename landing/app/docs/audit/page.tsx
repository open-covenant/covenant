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
        <code>OperatorTokenRotationRejected</code>
      </h3>
      <pre>
        <code>{`{
  "type":            "operator_token_rotation_rejected",
  "peer_display":    "guest@local",
  "peer_pubkey_b58": "9hYz…Lk6w"
}`}</code>
      </pre>
      <p>
        Emitted when <code>RotateOperatorToken</code> is rejected
        because the authenticated peer&apos;s pubkey does not match
        the operator identity. The gate is silent in v0 single-peer
        — only the operator can authenticate, so the rejection branch
        is dead code — and becomes load-bearing at Phase-1 multi-peer
        where a guest peer reaching this path is a probe worth
        surfacing on the operator&apos;s <code>/audit</code> feed.
        The issuer is the daemon identity (not the rejected peer) so
        the row passes the cross-peer audit-feed isolation filter,
        mirroring <code>AuthenticationFailed</code>&apos;s audience
        model. <code>peer_pubkey_b58</code> is the unforgeable
        identity — <code>display</code> is wire-supplied and a future
        attacker could register <code>user@local</code> against any
        pubkey, so the base58 form (matching{" "}
        <code>bs58::encode(peer.pubkey)</code>) is the durable probe
        attribution. Distinct from <code>AuthenticationFailed</code>{" "}
        (the peer authenticated successfully — they failed an
        authorization check, not authentication).
      </p>

      <h3>
        <code>OperatorPeersListRejected</code>
      </h3>
      <pre>
        <code>{`{
  "type":            "operator_peers_list_rejected",
  "peer_display":    "guest@local",
  "peer_pubkey_b58": "9hYz…Lk6w"
}`}</code>
      </pre>
      <p>
        Emitted when <code>ListPeers</code> is rejected because the
        authenticated peer&apos;s pubkey does not match the operator
        identity (the gate is{" "}
        <code>peer.pubkey != self.identity.pubkey</code>). The issuer
        is the daemon identity (not the rejected peer), mirroring{" "}
        <code>OperatorTokenRotationRejected</code>&apos;s
        daemon-as-issuer audience model: the row surfaces on the
        operator&apos;s own <code>/audit</code> feed for probe
        triage, and recording it against the rejected peer would
        (a) hide the probe from the operator under the cross-peer
        audit-feed isolation filter and (b) turn the rejected
        peer&apos;s own feed into a probe-was-logged oracle.{" "}
        <code>peer_pubkey_b58</code> is the unforgeable identity —
        <code>peer_display</code> is wire-supplied and a future
        attacker could register <code>user@local</code> against any
        pubkey, so the base58 form (matching{" "}
        <code>bs58::encode(peer.pubkey)</code>) is the durable probe
        attribution that survives operator grep through the audit
        log unmodified. Distinct from{" "}
        <code>CapabilityCheck</code> (no capability is checked — the
        gate is identity-pubkey equality) and from{" "}
        <code>AuthenticationFailed</code> (the peer authenticated
        successfully — they failed an authorization check, not
        authentication).
      </p>

      <h3>
        <code>OperatorPeerRevokeRejected</code>
      </h3>
      <pre>
        <code>{`{
  "type":            "operator_peer_revoke_rejected",
  "peer_display":    "guest@local",
  "peer_pubkey_b58": "9hYz…Lk6w"
}`}</code>
      </pre>
      <p>
        Emitted when <code>RevokePeer</code> is rejected because the
        authenticated peer&apos;s pubkey does not match the operator
        identity. Daemon-as-issuer audience model matching{" "}
        <code>OperatorTokenRotationRejected</code> and{" "}
        <code>OperatorPeersListRejected</code> — recording the
        rejection under the rejected peer would (a) hide the probe
        from the operator&apos;s <code>/audit</code> feed under the
        cross-peer audit-feed isolation filter and (b) turn the
        rejected peer&apos;s own feed into a probe-was-logged oracle.{" "}
        <code>peer_pubkey_b58</code> is the unforgeable identifier —
        <code>peer_display</code> is wire-supplied and a future
        attacker could register <code>user@local</code> against any
        pubkey, so the base58 form (matching{" "}
        <code>bs58::encode(peer.pubkey)</code>) is the durable probe
        attribution. Distinct from <code>PeerRevoked</code>{" "}
        (operator-issuer success row that carries{" "}
        <code>token_prefix</code> for the revoked entry) and from{" "}
        <code>PeerSelfRevokeBlocked</code> (operator-issuer
        fat-finger guard where the operator is both issuer and
        audience). Distinct from <code>CapabilityCheck</code> (no
        capability is checked — the gate is identity-pubkey equality)
        and from <code>AuthenticationFailed</code> (the peer
        authenticated successfully — they failed an authorization
        check, not authentication).
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

      <h3>
        <code>MemoryRepairApplied</code>
      </h3>
      <pre>
        <code>{`{
  "type":      "memory_repair_applied",
  "memory_id": "uuid",
  "action":    "detach_parent" | "delete_record" | "backfill_provenance",
  "mode":      "apply" | "dry_run",
  "changed":   true,
  "reason":    "operator-corrected receipt"
}`}</code>
      </pre>
      <p>
        Emitted when <code>covenantd::Server::repair_memory</code>{" "}
        completes a scoped repair against a single memory record. The
        full before/after record shape is returned to the caller
        through the repair response; the audit row keeps the durable
        who/what/why envelope without duplicating memory text into the
        audit log. The issuer is the requesting peer (operator-as-issuer
        audience — the recording path asserts the issuer&apos;s pubkey
        matches the acting peer&apos;s), recorded only after the
        dispatch-time capability check and the{" "}
        <code>memory.repair.{`<mode>`}</code> scope check both pass; if
        either fails the daemon emits a <code>CapabilityCheck</code> or{" "}
        <code>CapabilityScopeRejected</code> row instead, so a{" "}
        <code>MemoryRepairApplied</code> row never coexists with a
        rejection row for the same request. <code>action</code> and{" "}
        <code>mode</code> are parser-fixed snake_case tokens produced by{" "}
        <code>memory_repair_action</code> and{" "}
        <code>memory_repair_mode</code> in covenantd — the three valid{" "}
        <code>action</code> values are{" "}
        <code>&quot;detach_parent&quot;</code>,{" "}
        <code>&quot;delete_record&quot;</code>, and{" "}
        <code>&quot;backfill_provenance&quot;</code>; the two valid{" "}
        <code>mode</code> values are <code>&quot;apply&quot;</code> and{" "}
        <code>&quot;dry_run&quot;</code>. <code>changed</code> is{" "}
        <code>true</code> only when <code>mode</code> is{" "}
        <code>&quot;apply&quot;</code> <em>and</em> the planner reported
        a would-change outcome — a dry-run row always reports{" "}
        <code>changed: false</code>, and an apply that found the record
        already in the requested state also reports{" "}
        <code>changed: false</code>. The flag is the mutation-vs-no-op
        triage signal that distinguishes a repair that actually edited
        from one that found nothing to change; a refactor that{" "}
        <code>#[serde(default)]</code>-ed it would mask that signal.
        Distinct from <code>MemoryCompactionApplied</code> (bulk path
        with <code>deleted</code>, <code>stale_marked</code>, and{" "}
        <code>parents_detached</code> id lists; one row per compaction
        run rather than per record).
      </p>

      <h3>
        <code>MemoryCompactionApplied</code>
      </h3>
      <pre>
        <code>{`{
  "type":             "memory_compaction_applied",
  "mode":             "apply" | "dry_run",
  "changed":          true,
  "reason":           "operator-bounded compaction",
  "deleted":          ["uuid", "…"],
  "stale_marked":     ["uuid", "…"],
  "parents_detached": ["uuid", "…"]
}`}</code>
      </pre>
      <p>
        Emitted when <code>covenantd::Server::compact_memory</code>{" "}
        completes a bounded compaction run. The issuer is the requesting
        peer (operator-as-issuer audience), recorded only after the{" "}
        <code>memory.compact.{`<mode>`}</code> capability gate{" "}
        <em>and</em> a follow-up operator-identity equality check both
        pass — even a guest peer that somehow holds{" "}
        <code>memory.compact.apply</code> is rejected with an
        operator-identity error before any compaction runs, so a
        non-operator <code>issuer</code> on this row should be read as a
        regression. Capability-scope failures emit{" "}
        <code>CapabilityScopeRejected</code> instead, so a{" "}
        <code>MemoryCompactionApplied</code> row never coexists with a
        rejection row for the same request. <code>mode</code> shares the
        parser-fixed snake_case vocabulary produced by{" "}
        <code>memory_repair_mode</code> in covenantd —{" "}
        <code>&quot;apply&quot;</code> or{" "}
        <code>&quot;dry_run&quot;</code>. <code>deleted</code>,{" "}
        <code>stale_marked</code>, and <code>parents_detached</code>{" "}
        carry the id arrays that classify what the run touched: ids
        only, never memory text, so the audit stream stays redactable
        while operators retain enough to correlate against{" "}
        <code>memory plan-compaction</code> dry-run output.{" "}
        <code>changed</code> is the compaction-mutation-vs-no-op triage
        signal — a refactor that{" "}
        <code>#[serde(default)]</code>-ed any of the three id lists
        would let a malformed row decode with empty lists and erase
        which ids were touched (the test pin in{" "}
        <code>covenant-audit</code> documents this exact regression).
        Distinct from <code>MemoryRepairApplied</code> (single-record
        path keyed on a <code>memory_id</code> and one of three{" "}
        <code>action</code> tokens — bulk compaction reports id arrays
        instead, one row per run rather than per record).
      </p>

      <h3>
        <code>A2ARepairApplied</code>
      </h3>
      <pre>
        <code>{`{
  "type":           "a2_a_repair_applied",
  "task_id":        "uuid",
  "action":         "requeue" | "force_error" | "auto_requeue",
  "reason":         "operator-cleared stuck lease",
  "lease_id":       "uuid",
  "duplicate_risk": "idempotent" | "low" | "high" | null,
  "attempt":        1
}`}</code>
      </pre>
      <p>
        Emitted when <code>covenantd::Server</code> repairs an in-flight
        A2A mailbox lease — either via an operator{" "}
        <code>RepairA2A</code> request after the{" "}
        <code>a2a.repair</code> scope check passes, or by the
        disabled-by-default A2A auto-retry scheduler running as the
        operator peer. Full task payloads stay in the mailbox log; the
        audit row records who acted, why, and which lease they intended
        to mutate. The issuer is the requesting peer
        (operator-as-issuer audience). <code>action</code> is{" "}
        <code>&quot;requeue&quot;</code> or{" "}
        <code>&quot;force_error&quot;</code> for operator-issued repairs
        and <code>&quot;auto_requeue&quot;</code> for scheduler-issued
        ones — the slug is the only triage signal that separates a
        scheduler-driven retry from an operator-driven one on the
        operator&apos;s <code>/audit</code> feed.{" "}
        <code>attempt</code> is the lease&apos;s retry-count after the
        repair lands, which distinguishes a first repair (<code>1</code>)
        from a re-repair on the same lease.
      </p>
      <p>
        The wire-form <code>type</code> discriminator is{" "}
        <code>&quot;a2_a_repair_applied&quot;</code>, <em>not</em>{" "}
        <code>a2a_repair_applied</code> — serde&apos;s{" "}
        <code>rename_all = &quot;snake_case&quot;</code> splits the{" "}
        <code>A2A</code> prefix on each digit/uppercase boundary so the
        durable on-disk slug carries the extra underscore. A refactor
        that &quot;fixed&quot; the slug to{" "}
        <code>a2a_repair_applied</code> would silently strand every
        prior A2A-lease-repair audit row at decode time, which is why
        the wire shape is pinned in <code>covenant-audit</code>.{" "}
        <code>lease_id</code> and <code>duplicate_risk</code> are{" "}
        <code>Option</code> fields but carry no{" "}
        <code>#[serde(skip_serializing_if)]</code>, so both keys always
        surface on the wire — <code>null</code> when the daemon did not
        bind them. The wire shape stays at exactly seven keys across
        every repair row regardless of whether the daemon classified the
        lease&apos;s duplicate-risk; a doc example that elided either
        key for the None case would let a future{" "}
        <code>skip_serializing_if</code> regression masquerade as the
        documented contract. Distinct from{" "}
        <code>MemoryRepairApplied</code> (different subject: an A2A
        mailbox lease, not a memory record — the audit-feed audience
        and scope vocabularies are operator-only on both, but
        cross-feed joins should key on this slug, not on the
        peer-event audience).
      </p>

      <h3>
        <code>A2AAutoRetrySchedulerScan</code>
      </h3>
      <pre>
        <code>{`{
  "type":              "a2_a_auto_retry_scheduler_scan",
  "enabled":           true,
  "considered":        10,
  "requeued":          2,
  "skipped":           8,
  "skipped_by_reason": { "max_attempts": 5, "lease_age": 3 },
  "min_lease_age_ms":  30000,
  "max_attempts":      3,
  "max_requeues":      100,
  "scan_limit":        200,
  "error":             null
}`}</code>
      </pre>
      <p>
        Emitted by the disabled-by-default A2A retry scheduler after
        each scan run. Requeued tasks still surface as individual{" "}
        <code>A2ARepairApplied</code> rows with{" "}
        <code>action: &quot;auto_requeue&quot;</code>; this summary row
        makes skipped and rejected scans visible without duplicating
        per-task payloads. The issuer is the{" "}
        <em>daemon&apos;s</em> identity (the recording path uses{" "}
        <code>record_daemon_event</code>, not{" "}
        <code>record_peer_event</code>), so the audience model mirrors{" "}
        <code>AuthenticationFailed</code> rather than{" "}
        <code>A2ARepairApplied</code> — even though both A2A rows share
        the <code>a2_a_</code> slug stem, joins that key on the
        peer-event audience will miss every scan row. The policy
        snapshot fields (<code>enabled</code>,{" "}
        <code>min_lease_age_ms</code>, <code>max_attempts</code>,{" "}
        <code>max_requeues</code>, <code>scan_limit</code>) record the
        configuration the scan ran under, so an operator can correlate a
        sudden requeued-count change with a policy edit without rereading
        the daemon log. <code>skipped_by_reason</code> is a JSON object
        keyed by skip-reason with per-reason counts — it makes the
        operator-misconfig-vs-policy-gate breakdown legible without
        landing the full task payloads on the audit stream. The wire
        slug is <code>&quot;a2_a_auto_retry_scheduler_scan&quot;</code>,{" "}
        <em>not</em> <code>a2a_auto_retry_scheduler_scan</code> — the
        same serde <code>rename_all = &quot;snake_case&quot;</code>{" "}
        digit/upper split that <code>A2ARepairApplied</code> carries,
        pinned in <code>covenant-audit</code>. <code>error</code> is{" "}
        <code>Option&lt;String&gt;</code> without{" "}
        <code>#[serde(skip_serializing_if)]</code>, so the key surfaces
        as JSON <code>null</code> on success scans and the eleven-key
        wire shape stays stable across success and failure rows.
        Distinct from <code>A2ARepairApplied</code> (one summary row per
        scan vs one row per repaired lease — joining the two surfaces by{" "}
        <code>task_id</code> reconstructs which leases the scan
        touched).
      </p>

      <h3>
        <code>A2ARecipientRejected</code>
      </h3>
      <pre>
        <code>{`{
  "type":              "a2_a_recipient_rejected",
  "sender_display":    "attacker@local",
  "recipient_display": "victim@local",
  "action":            "a2a.recv.attacker@local"
}`}</code>
      </pre>
      <p>
        Emitted when <code>SendA2ATask</code> is rejected because the
        recipient peer has not granted{" "}
        <code>a2a.recv.{`<sender>`}</code> to themselves. The gate
        closes the recipient-inbox spam vector that would otherwise be
        exploitable when a peer with a granted send-cap pushes tasks at
        a recipient that has not granted matching recv-caps: without
        it, a malicious peer could route arbitrary{" "}
        <code>intent_text</code> into the recipient&apos;s{" "}
        <code>RecentA2ATasks</code> view via the bidirectional filter.
        The issuer is the <em>sender</em> peer (their failed attempt)
        — but the missing cap belongs to a <em>different subject</em>,
        the recipient, which is the entire reason the variant exists as
        its own kind rather than as a <code>CapabilityCheck</code> row.
        Both <code>sender_display</code> and{" "}
        <code>recipient_display</code> are load-bearing for the
        two-party diagnostic; <code>action</code> is the formatted
        scope name (<code>a2a.recv.{`<sender_display>`}</code>) that the
        recipient never granted, which lets an operator triaging the
        row pivot directly to the recipient&apos;s grant decisions
        without recomputing the missing-cap string. The wire-form{" "}
        <code>type</code> slug is{" "}
        <code>&quot;a2_a_recipient_rejected&quot;</code>, <em>not</em>{" "}
        <code>a2a_recipient_rejected</code> — the same serde{" "}
        <code>rename_all = &quot;snake_case&quot;</code> A2A
        digit/upper split pinned in <code>covenant-audit</code>.
        Distinct from <code>CapabilityCheck</code> with{" "}
        <code>passed: false</code> (which would misattribute the
        missing cap to the issuer&apos;s capability set rather than
        the recipient&apos;s) and from <code>A2ASenderMismatch</code>{" "}
        (sender-identity mismatch on the sender&apos;s own caps, not a
        recipient-side cap gap).
      </p>

      <h3>
        <code>A2ASenderMismatch</code>
      </h3>
      <pre>
        <code>{`{
  "type":                   "a2_a_sender_mismatch",
  "peer_display":           "attacker@local",
  "claimed_sender_display": "victim@local"
}`}</code>
      </pre>
      <p>
        Emitted when <code>SendA2ATask</code> is rejected because the
        supplied <code>task.sender</code> does not match the
        authenticated peer on the connection. The gate closes the
        sender-spoof attack class — a malicious local process claiming
        to be a different agent on the wire than the one bound to its
        peer token. The check fires as a precondition before any cap
        check runs, so a spoof attempt never even reaches{" "}
        <code>A2ARecipientRejected</code> or the{" "}
        <code>CapabilityCheck</code> path on the same{" "}
        <code>SendA2ATask</code>. The issuer is the actual{" "}
        <em>authenticated</em> peer (the recording path uses{" "}
        <code>peer.clone()</code> against{" "}
        <code>record_peer_event_required</code>) — not the spoofed
        identity, so an attacker can&apos;t hide the attempt behind the
        impersonated peer&apos;s audit feed. <code>peer_display</code>{" "}
        and <code>claimed_sender_display</code> are both load-bearing
        for the two-identity diagnostic; a{" "}
        <code>#[serde(default)]</code> on either would collapse the
        spoof event into a one-sided diagnostic and lose the
        attribution. The wire-form <code>type</code> slug is{" "}
        <code>&quot;a2_a_sender_mismatch&quot;</code>, <em>not</em>{" "}
        <code>a2a_sender_mismatch</code> — the same serde{" "}
        <code>rename_all = &quot;snake_case&quot;</code> A2A
        digit/upper split. Distinct from{" "}
        <code>A2ARecipientRejected</code> (post-spoof-gate path — a
        recipient-side cap gap on an honestly-attributed sender) and
        from <code>AuthenticationFailed</code> (no peer authenticated;
        no <code>task.sender</code> claim to compare against).
      </p>

      <h3>
        <code>A2AResultRejected</code>
      </h3>
      <pre>
        <code>{`{
  "type":    "a2_a_result_rejected",
  "task_id": "uuid",
  "reason":  "unknown_task"
}`}</code>
      </pre>
      <p>
        Emitted when <code>PostA2AResult</code> is rejected upstream of
        any capability check — currently when{" "}
        <code>mailbox.lookup_task_sender(task_id)</code> returns no
        sender, meaning the supplied <code>task_id</code> was never
        dispatched through this daemon. The reason field carries the
        literal <code>&quot;unknown_task&quot;</code> for that path.
        Fires before the <code>a2a.respond</code> cap check would even
        run, so an <code>A2AResultRejected</code> row never coexists
        with a <code>CapabilityCheck</code> for the same{" "}
        <code>PostA2AResult</code> request. The issuer is the
        authenticated peer that submitted the bogus{" "}
        <code>task_id</code> (peer-as-issuer audience via{" "}
        <code>record_peer_event</code>) — the peer&apos;s own audit
        feed surfaces the row so a benign client bug stays visible to
        its operator without leaking the rejection to other peers. The
        variant is a stronger compromise indicator than a missing-cap
        rejection: an honest agent&apos;s <code>a2a.respond</code>{" "}
        would carry a <code>task_id</code> that was actually dispatched
        through this daemon, so a missing-task row implies the agent
        fabricated the id or replayed one from a different daemon. The
        wire-form <code>type</code> slug is{" "}
        <code>&quot;a2_a_result_rejected&quot;</code>, <em>not</em>{" "}
        <code>a2a_result_rejected</code> — the same serde{" "}
        <code>rename_all = &quot;snake_case&quot;</code> A2A
        digit/upper split. Distinct from <code>CapabilityCheck</code>{" "}
        with <code>passed: false</code> (which assumes the{" "}
        <code>task_id</code> is valid and only the cap is short — a
        refactor that moved the unknown-task gate behind the cap
        check would silently absorb this stronger signal into a
        routine cap miss) and from <code>A2ASenderMismatch</code>{" "}
        (sender-spoof at the send path rather than result-injection at
        the respond path).
      </p>

      <h3>
        <code>HermesToolInvoked</code>
      </h3>
      <pre>
        <code>{`{
  "type":             "hermes_tool_invoked",
  "intent_id":        "uuid",
  "run_id":           "run_abc",
  "tool":             "terminal",
  "preview_hash_hex": "8f1c…0c2e"
}`}</code>
      </pre>
      <p>
        Emitted by <code>covenantd</code>&apos;s runtime-trace fold (
        <code>runtime_trace_to_audit_kind</code>) when a Hermes-runtime
        agent starts a tool invocation inside its run loop.{" "}
        <code>intent_id</code> stamps the parent intent so the audit
        row ties back to the broader intent context — a refactor that
        dropped the stamping would strand every Hermes tool invocation
        from the originating <code>IntentDispatched</code> row.{" "}
        <code>run_id</code> is the Hermes-side run identifier and is
        the grouping key that pairs this row with the matching{" "}
        <code>HermesToolCompleted</code> row (joining on{" "}
        <code>intent_id + run_id + tool</code> reconstructs the
        tool-call duration). <code>preview_hash_hex</code> is the
        SHA-256 of Hermes&apos;s short tool-input preview — the raw
        preview text is hashed via <code>covenant_audit::hash_hex</code>{" "}
        before persisting, so the audit chain never embeds raw tool
        input. That redaction floor is load-bearing: a refactor that
        &quot;simplified&quot; by passing the raw preview through
        (e.g., under a &quot;preview already operator-facing&quot;
        rationale) would silently leak every Hermes tool-input
        preview verbatim into the persisted audit chain, which is why
        the wire form pins <code>preview_hash_hex</code> rather than
        any unhashed alternative. Distinct from{" "}
        <code>IntentDispatched</code> (one row per intent — Hermes runs
        emit many <code>HermesToolInvoked</code> rows per intent, one
        per tool call) and from <code>HermesToolCompleted</code>{" "}
        (end-of-tool row carrying <code>duration_ms</code> and{" "}
        <code>error</code>; pair them by run_id + tool + intent_id).
      </p>

      <h3>
        <code>HermesToolCompleted</code>
      </h3>
      <pre>
        <code>{`{
  "type":        "hermes_tool_completed",
  "intent_id":   "uuid",
  "run_id":      "run_abc",
  "tool":        "terminal",
  "duration_ms": 1234,
  "error":       false
}`}</code>
      </pre>
      <p>
        Emitted when a tool invocation in a Hermes run finishes. The
        end-of-tool counterpart to <code>HermesToolInvoked</code> — pair
        them by <code>intent_id + run_id + tool</code> to reconstruct
        tool-call latency. <code>duration_ms</code> is the latency the
        audit row carries verbatim from the runtime trace; operators
        key Hermes latency dashboards on this field, so a refactor that
        coerced to a different width or unit would silently shift every
        dashboard built against milliseconds. <code>error</code> is{" "}
        <code>true</code> iff the tool itself raised — a{" "}
        <code>false</code> here followed by a failed overall run status
        means Hermes failed elsewhere in the loop (model error, policy
        denial, transport issue), so this flag is the only reliable
        per-tool failure signal on the audit feed. A default-to-false
        regression would mask every tool failure as success; the
        invariant is pinned in <code>covenant-audit</code>.{" "}
        <code>intent_id</code> stamps the parent intent just like{" "}
        <code>HermesToolInvoked</code>, so tool durations remain
        attributable to the originating <code>IntentDispatched</code>{" "}
        even after the run terminates. Distinct from{" "}
        <code>HermesToolInvoked</code> (start-of-tool with{" "}
        <code>preview_hash_hex</code> vs end-of-tool with{" "}
        <code>duration_ms</code> and <code>error</code>) and from{" "}
        <code>HermesApprovalRequested</code> (a run pausing for
        operator approval is not a tool completion — approval rows do
        not carry <code>tool</code> or <code>duration_ms</code>).
      </p>

      <h3>
        <code>HermesApprovalRequested</code>
      </h3>
      <pre>
        <code>{`{
  "type":      "hermes_approval_requested",
  "intent_id": "uuid",
  "run_id":    "run_abc",
  "choices":   ["allow", "deny"]
}`}</code>
      </pre>
      <p>
        Emitted when a Hermes run pauses pending operator approval —
        recorded so a run stalled at the approval prompt stays
        auditable even when the operator console is closed.{" "}
        <code>intent_id</code> stamps the parent intent so the audit
        feed surfaces a stuck approval against the originating{" "}
        <code>IntentDispatched</code>. <code>run_id</code> is the same
        Hermes-side run identifier the tool rows carry and pairs this
        request half with the matching{" "}
        <code>HermesApprovalResolved</code> row that records the
        operator&apos;s answer. <code>choices</code> is the ordered
        list Hermes presents to the operator; the runtime-trace fold
        passes the vector through verbatim in order because operator
        approval UIs render the list in audit-supplied order, so a
        sort or dedup pass at this layer would silently reorder every
        operator&apos;s approval prompt across deployments (the
        invariant is pinned in <code>covenant-audit</code>). Distinct
        from <code>HermesToolInvoked</code> and{" "}
        <code>HermesToolCompleted</code> (the approval pause is not a
        tool — there is no <code>tool</code> or{" "}
        <code>preview_hash_hex</code> or <code>duration_ms</code>) and
        from <code>HermesApprovalResolved</code> (start-of-pause vs
        end-of-pause; the pair shares{" "}
        <code>intent_id + run_id</code>, but the resolved row adds the
        chosen <code>choice</code> and the <code>resolved</code> count
        Hermes reports as cleared by the response).
      </p>

      <h3>
        <code>HermesApprovalResolved</code>
      </h3>
      <pre>
        <code>{`{
  "type":      "hermes_approval_resolved",
  "intent_id": "uuid",
  "run_id":    "run_abc",
  "choice":    "allow",
  "resolved":  1
}`}</code>
      </pre>
      <p>
        Emitted when an operator (or auto-policy) answers a pending
        Hermes approval — the end-of-pause counterpart to{" "}
        <code>HermesApprovalRequested</code>, paired by{" "}
        <code>intent_id + run_id</code> so operators reconstruct
        approval latency by joining the two rows.{" "}
        <code>intent_id</code> stamps the parent intent so the
        resolution stays attributable to the originating{" "}
        <code>IntentDispatched</code> even after the run terminates.{" "}
        <code>run_id</code> is the same Hermes-side run identifier the
        request row carries. <code>choice</code> is the selected
        string from the originally presented{" "}
        <code>choices</code> vector — the invariant is that the value
        matches one of the entries the request row recorded, so a
        free-form unrelated string here would silently break
        approval-lifecycle reconstruction across deployments.{" "}
        <code>resolved</code> is the count of pending requests Hermes
        reports as cleared by this response, recorded as the
        per-response cleared count (not a running total). An
        accumulator regression that summed across rows would make
        every operator&apos;s approval dashboard count duplicates,
        which is why the wire form pins the per-response semantic
        rather than any running-sum alternative. Distinct from{" "}
        <code>HermesApprovalRequested</code> (start-of-pause carrying
        the <code>choices</code> vector vs end-of-pause carrying the
        single selected <code>choice</code> and the{" "}
        <code>resolved</code> count) and from{" "}
        <code>HermesToolCompleted</code> (an approval response is not
        a tool completion — there is no <code>tool</code>,{" "}
        <code>duration_ms</code>, or <code>error</code> field).
      </p>

      <h3>
        <code>SettlementReceiptBackfillApplied</code>
      </h3>
      <pre>
        <code>{`{
  "type":          "settlement_receipt_backfill_applied",
  "row_count":     3,
  "rollback_path": "/home/op/receipts/working.jsonl.bak",
  "dry_run":       false
}`}</code>
      </pre>
      <p>
        Emitted when{" "}
        <code>covenantd::Server::backfill_settlement_receipts</code>{" "}
        completes the authorized settlement receipt backfill mutation.
        The issuer is the requesting peer (operator-as-issuer audience),
        recorded only after the dispatch-time{" "}
        <code>settlement.backfill.{`<mode>`}</code> capability check, a
        follow-up operator-identity equality check, and the{" "}
        <code>settlement-backfill</code> capability scope check all
        pass; capability-scope failures emit{" "}
        <code>CapabilityScopeRejected</code> instead, so a{" "}
        <code>SettlementReceiptBackfillApplied</code> row never coexists
        with a rejection row for the same request. The row is emitted
        only after <code>backfill_receipts</code> returned{" "}
        <code>Ok</code> — i.e. after the rollback checkpoint, the
        rewritten store contents, and the renamed store file are
        fsynced — so the audit log cannot claim a mutation whose data
        did not durably land. <code>row_count</code> is the count of
        legacy rows the backfill plan would change on a dry run and the
        count it actually rewrote on an apply; an apply that found
        nothing to change reports <code>row_count: 0</code> and the
        mutator short-circuits without writing a rollback file.{" "}
        <code>dry_run</code> is the plan-vs-mutation triage signal: a
        dry-run row always carries <code>dry_run: true</code> and{" "}
        <code>rollback_path: null</code>, and a refactor that{" "}
        <code>#[serde(default)]</code>-ed either field would let an
        applied rewrite masquerade as a dry run or mask the
        applied-vs-planned distinction. <code>rollback_path</code> is
        the absolute path of the pre-rewrite checkpoint the mutator
        wrote alongside the receipts store on an apply,{" "}
        <code>null</code> on a dry run or a no-op apply that changed
        nothing; the field carries no{" "}
        <code>#[serde(skip_serializing_if)]</code> so the wire form is
        always four keys (the key stays present as <code>null</code>{" "}
        when None) — a consumer that filters on the applied-vs-dry
        split reads <code>dry_run</code> while one that wants the
        rollback checkpoint reads <code>rollback_path</code>, so both
        must stay on the wire across the Some and None cases. The row
        is best-effort like every other completed-mutation kind — the
        rewrite is already durable and the rollback file is on disk,
        so audit-write success is not a precondition for the response
        (the variant is intentionally absent from{" "}
        <code>audit_kind_requires_persistence</code>, which is reserved
        for suppressible rejection probes whose suppression would hide
        an attacker probe). Distinct from{" "}
        <code>MemoryRepairApplied</code> and{" "}
        <code>MemoryCompactionApplied</code> (different subject: the
        settlement receipt store rewrite vs a memory-record mutation —
        the rollback evidence here is an on-disk checkpoint sibling of
        the store rather than a SQLite savepoint or per-record
        before/after payload, and the action vocabulary collapses to
        the single backfill operation rather than the three repair
        actions or the three id-array categories compaction reports).
      </p>

      <h3>
        <code>MemoryRecordBackfillApplied</code>
      </h3>
      <pre>
        <code>{`{
  "type":           "memory_record_backfill_applied",
  "row_count":      3,
  "savepoint_name": "backfill_receipt_correlation",
  "dry_run":        false
}`}</code>
      </pre>
      <p>
        Emitted when{" "}
        <code>covenantd::Server::backfill_memory_record_correlations</code>{" "}
        completes the authorized memory-record receipt-correlation
        backfill mutation. The issuer is the requesting peer
        (operator-as-issuer audience), recorded only after the
        dispatch-time <code>memory.backfill.{`<mode>`}</code> capability
        check, a follow-up operator-identity equality check, and the{" "}
        <code>memory-backfill</code> capability scope check all pass;
        capability-scope failures emit{" "}
        <code>CapabilityScopeRejected</code> instead, so a{" "}
        <code>MemoryRecordBackfillApplied</code> row never coexists with
        a rejection row for the same request. The row is emitted only
        after{" "}
        <code>SqliteStore::backfill_receipt_correlation</code> returned{" "}
        <code>Ok</code> — i.e. after the{" "}
        <code>BEGIN IMMEDIATE</code> +{" "}
        <code>SAVEPOINT backfill_receipt_correlation</code> + per-row{" "}
        <code>UPDATE</code> + <code>RELEASE SAVEPOINT</code> +{" "}
        <code>COMMIT</code> all succeed — so the audit log cannot claim
        a mutation whose data did not durably land.{" "}
        <code>row_count</code> is the count of memory records the
        planner would correlate on a dry run and the count actually
        rewritten on an apply; an apply that found nothing to change
        (all correlations matched the existing{" "}
        <code>metadata.receipt_id</code>) reports{" "}
        <code>row_count: 0</code> and the mutator short-circuits without
        leaving any visible savepoint behind.{" "}
        <code>dry_run</code> is the plan-vs-mutation triage signal: a
        dry-run row always carries <code>dry_run: true</code> and{" "}
        <code>savepoint_name: null</code>, and a refactor that{" "}
        <code>#[serde(default)]</code>-ed either field would let an
        applied rewrite masquerade as a dry run or mask the
        applied-vs-planned distinction. <code>savepoint_name</code> is
        the SQLite SAVEPOINT identifier the mutator wrapped the apply
        batch in (the constant{" "}
        <code>backfill_receipt_correlation</code> exported as{" "}
        <code>covenant_memory::MEMORY_BACKFILL_SAVEPOINT_NAME</code>),{" "}
        <code>null</code> on a dry run or a no-op apply that changed
        nothing; the field carries no{" "}
        <code>#[serde(skip_serializing_if)]</code> so the wire form is
        always four keys (the key stays present as <code>null</code>{" "}
        when None) — a consumer that filters on the applied-vs-dry
        split reads <code>dry_run</code> while one that wants the
        SAVEPOINT identifier reads <code>savepoint_name</code>, so both
        must stay on the wire across the Some and None cases, and the
        stable column set is what lets operator dashboards JOIN this
        row with <code>SettlementReceiptBackfillApplied</code> under
        the same backfill-family schema. The row is best-effort like
        every other completed-mutation kind — the SAVEPOINT-wrapped
        batch already <code>COMMIT</code>ted, so audit-write success is
        not a precondition for the response (the variant is
        intentionally absent from{" "}
        <code>audit_kind_requires_persistence</code>, which is reserved
        for suppressible rejection probes whose suppression would hide
        an attacker probe). Distinct from{" "}
        <code>SettlementReceiptBackfillApplied</code> (the two-sided
        counterpart writes <code>memory_record_id</code> onto the
        receipts JSONL store under an on-disk rollback checkpoint
        rather than{" "}
        <code>metadata.receipt_id</code> onto memory records under a
        SQLite SAVEPOINT — same correlation operation, opposite
        direction and different rollback mechanism), and from{" "}
        <code>MemoryRepairApplied</code> and{" "}
        <code>MemoryCompactionApplied</code> (different action
        vocabulary: the receipt-correlation backfill writes the{" "}
        <code>receipt_id</code> field into metadata rather than
        detaching parents, deleting records, backfilling provenance,
        or marking long-term entries stale, and the rollback evidence
        here is a SAVEPOINT identifier rather than per-record
        before/after payloads or id-array categories).
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
