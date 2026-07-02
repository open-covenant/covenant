---
description: Show the latest covguard run, spend against the cap, outcome, and receipt.
argument-hint: "[status | verify | doctor]"
---

The user is asking about covguard, the local guard that runs coding agents
under a spend cap, a sandbox, and a signed receipt. Run the matching command
and report the result plainly. Lead with the outcome and the spend against the
cap.

Argument: $ARGUMENTS

- (empty) or `status` → run `covguard receipts show last`
- `verify` → run `covguard receipts show last` to find the receipt path, then
  `covguard verify <that receipt.json>`
- `doctor` → run `covguard doctor`

If `covguard` is not installed, tell the user to install it
(`curl -fsSL https://opencovenant.org/guard/install.sh | sh`) and stop. Do not
attempt to run agents unguarded on their behalf.
