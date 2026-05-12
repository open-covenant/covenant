# examples

Runnable Covenant agents. Each subdirectory ships an `agent.toml` manifest, an entry point, and a short README explaining which primitives the example exercises.

| Example                                | Primitives                                  | Network | Sandbox |
|----------------------------------------|---------------------------------------------|---------|---------|
| [`hello-agent`](./hello-agent/)        | Intent, Permissions, Runtime, Audit         | off     | trusted-local |

More examples land here over time. The shape of each example is intentionally small — the goal is to exercise the *surrounding* daemon protocol, not the agent logic.
