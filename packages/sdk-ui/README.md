# @covenant/sdk-ui

React hooks for Covenant's Solana-native SDK surfaces.

| Hook | Purpose |
| --- | --- |
| `useSolanaSignIn()` | Connect a Solana wallet, sign a nonce, and establish a session. |
| `useSession()` | Read the current session (`{ address, expiresAt }` or `null`). |
| `useSignOut()` | Clear the session and invalidate the cached session query. |
| `useCredits()` | Read local credit summaries. |
| `useBuyCredits()` | Prepare a client-side credit purchase action payload. |
| `useAllowedMints()` | Read the configured stable-coin mints accepted for credit purchases. |
| `useLeaderboard()` | Read agent ranking fixtures. |

Requires `QueryClientProvider` and a Solana wallet capable of message signing.

## Stability

This package is a private workspace-alpha UI SDK surface. `compatibility/exports.v1.json` tracks hook export drift, but the package must stay private until public React hook stability, peer dependency policy, npm publication, and rollback/deprecation policy are approved.
