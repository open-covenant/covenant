# GitHub Apps

Mizuki uses three separate GitHub Apps. Their keys and installations must never be shared.

| App             | Visibility | Installation scope                              | Authority                                                        |
| --------------- | ---------- | ----------------------------------------------- | ---------------------------------------------------------------- |
| Core            | Public     | Each public repository whose maintainer opts in | Read issues and checks; deliver pull requests on Mizuki branches |
| Policy Verifier | Public     | The same opted-in repository                    | Exactly read-only contents, issues, metadata, and pull requests  |
| Updater         | Private    | `open-covenant/covenant` only                   | Create and merge reviewed capability-update pull requests        |

## Registration

Registration is an authenticated GitHub account action and is intentionally not automated by deployment code.

1. Verify `https://mizuki.covenant.org`, including the OAuth callback and signed-webhook proxy path.
2. Open GitHub **Settings → Developer settings → GitHub Apps → New GitHub App from a manifest**.
3. Submit the matching `*.manifest.json` file without adding permissions or events.
4. Generate one private key per App and store it directly in the production secret manager. Never download it into the repository.
5. Set the App ID and RSA key on the owning service. Core also receives its OAuth client secret and webhook secret.
6. Publish the Core and Policy Verifier installation links. An external maintainer must select only the repository they are authorizing.
7. Keep the Updater private and install it only on the protected application repository.

Before paid intake, verify both public installations independently. Core must obtain a repository token with its exact delivery permissions. The signer must authenticate the Policy Verifier App, discover the installation for that exact repository, mint a short-lived token with the manifest's exact four read permissions, and reject missing or additional permissions. Removing either installation must make new work fail closed.

Record only App IDs, slugs, installation IDs, repository selections, and public installation URLs in release evidence. Private keys, OAuth secrets, webhook secrets, and installation tokens never belong in logs or evidence files.
