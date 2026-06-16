# examples

Runnable Covenant agents. Each subdirectory ships an `agent.toml` manifest, an entry point, and a short README explaining which primitives the example exercises.

| Example                                | Primitives                                  | Network | Sandbox |
|----------------------------------------|---------------------------------------------|---------|---------|
| [`hello-agent`](./hello-agent/)        | Intent, Permissions, Runtime, Audit         | off     | trusted-local |

More examples land here over time. The shape of each example is intentionally small — the goal is to exercise the *surrounding* daemon protocol, not the agent logic.

## SDK client examples

The agents above are spawned *by* the daemon over stdin/stdout. To build a program that connects *to* a running daemon over IPC, see the [`covenant-sdk`](../agent-os/crates/covenant-sdk/examples) client examples — `tool_agent`, `memory_agent`, and `a2a_worker` — and [docs/agent-sdk.md](../docs/agent-sdk.md).
