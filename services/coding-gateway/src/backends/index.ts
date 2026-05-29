import type { BackendId, CodingBackend } from "../types.js";
import { AnthropicBackend } from "./anthropic.js";

/** Select a coding backend by id. OpenAI lands in coder-05. */
export function selectBackend(id: BackendId = "anthropic"): CodingBackend {
  switch (id) {
    case "anthropic":
      return new AnthropicBackend();
    case "openai":
      throw new Error("openai backend is not implemented yet (coder-05)");
    default:
      throw new Error(`unknown backend: ${id satisfies never}`);
  }
}
