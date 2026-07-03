# MCP registry packaging

`server.json` is the published MCP Registry entry (`org.opencovenant/guard`, DNS-auth
namespace on opencovenant.org). `manifest.json` is the MCPB bundle manifest; the bundle
is a zip of `manifest.json` + `server/covguard-darwin-arm64` + `server/covguard-linux-x64`
(binaries from the release tarballs), attached to the GitHub release as
`covenant-guard.mcpb`.

To publish a new version: rebuild the bundle from the new release binaries, upload it to
the release, update `version` and `fileSha256` in `server.json`, then
`mcp-publisher login dns --domain opencovenant.org --private-key <hex>` and
`mcp-publisher publish`. The signing key lives at `~/.config/covenant-mcp-dns-key.pem`;
the TXT record `v=MCPv1; k=ed25519; p=...` sits on the opencovenant.org apex.
