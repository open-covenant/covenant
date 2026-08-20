import { clean } from "../agentStream.mjs";

// Substitute a public label when an author name or email trips the identity
// leak-token scan, so historical commits still render on the public witness
// surface without leaking an operator identity. Fail-closed on either field: if
// name OR email carries a banned token (clean returns null), both are replaced —
// never one passed through while the other is scrubbed.
export function redactAuthor(name: string, email: string): { display: string; email: string } {
  if (clean(name) === null || clean(email) === null) {
    return { display: "Covenant Legacy", email: "legacy@opencovenant.org" };
  }
  return { display: name, email };
}
