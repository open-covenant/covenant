# @covenant/sdk-ui

React hooks for Covenant's Solana-native SDK surfaces.

| Hook | Purpose |
| --- | --- |
| `useSolanaSignIn()` | Connect a Solana wallet, sign a nonce, and establish a session. |
| `useCredits()` | Read local credit summaries. |
| `useBuyCredits()` | Prepare a client-side credit purchase action payload. |
| `useLeaderboard()` | Read agent ranking fixtures. |

Requires `QueryClientProvider` and a Solana wallet capable of message signing.

## Stability

This package is a private workspace-alpha UI SDK surface. Keep it private until public React hook stability, peer dependency policy, npm publication, and rollback/deprecation policy are approved.
