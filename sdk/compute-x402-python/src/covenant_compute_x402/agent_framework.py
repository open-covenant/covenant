"""Microsoft Agent Framework tool for one application-authorized purchase.

Import this optional module after installing the ``agent-framework`` extra.
The application must save request_id before registering the tool and reuse that
identity, the exact arguments and the client journal after a restart.
"""
from __future__ import annotations

import json
from typing import Any
from uuid import UUID

from agent_framework import FunctionTool
from pydantic import BaseModel, ConfigDict, Field

from .client import CovenantX402Client, validate_request


class InferenceArguments(BaseModel):
    """The complete model-visible surface. Payment policy is never a model input."""

    model_config = ConfigDict(extra="forbid", strict=True)
    prompt: str = Field(min_length=1, max_length=64_000, description=(
        "The complete task for the configured inference model. Reuse the identical "
        "task when resuming this purchase."
    ))


def compact_result(result: dict[str, Any]) -> str:
    """Keep the signed envelope in the client journal, outside planner context."""
    content = {"status": result["status"], "requestId": result["requestId"]}
    if result["status"] == "completed":
        content.update(text=result["text"], amountMicroUsdc=result["amountMicroUsdc"],
                       transaction=result["settlement"]["transaction"], receiptVerified=True)
    else:
        content["requestSha256"] = result["requestSha256"]
    return json.dumps(content, ensure_ascii=False, separators=(",", ":"))


def covenant_inference_tool(
    *, client: CovenantX402Client, request_id: str, model: str,
    max_tokens: int = 2048, temperature: float | None = 0.2,
    system_prompt: str | None = None, checkpoint_before_dispatch: bool = False,
) -> FunctionTool:
    """Return an official FunctionTool with prompt-only, strict arguments.

    ``checkpoint_before_dispatch`` is an application-controlled planning gate:
    Agent.run emits an approval request instead of calling the paid tool. The
    included workflow persists that selected call before the application resumes
    it. It is not a second wallet or spending authorization mechanism.

    Direct ``await tool.invoke(arguments={"prompt": ...}, skip_parsing=True)``
    returns the complete verified purchase; normal Agent invocation receives the
    compact answer/payment summary. A pending purchase is never a final answer.
    """
    if not isinstance(request_id, str) or str(UUID(request_id)) != request_id:
        raise ValueError("trusted_stable_uuid_required")
    # Use the native client's exact validation rules during registration too.
    fixed = validate_request(model, [
        *([{"role": "system", "content": system_prompt}] if system_prompt is not None else []),
        {"role": "user", "content": "configuration validation"},
    ], max_tokens, temperature)
    if not callable(getattr(client, "purchase", None)):
        raise TypeError("covenant_x402_client_required")

    # Immutable scalar copies of all application-owned policy. Neither model
    # arguments nor later mutation of caller configuration can replace them.
    fixed_request_id, fixed_model = request_id, fixed["model"]
    fixed_max_tokens, fixed_temperature = fixed["max_tokens"], fixed.get("temperature")
    fixed_system_prompt = fixed["messages"][0]["content"] if system_prompt is not None else None

    async def invoke(prompt: str) -> dict[str, Any]:
        args = InferenceArguments.model_validate({"prompt": prompt})
        messages = ([{"role": "system", "content": fixed_system_prompt}]
                    if fixed_system_prompt is not None else [])
        messages.append({"role": "user", "content": args.prompt})
        return await client.purchase(request_id=fixed_request_id, model=fixed_model,
                                     messages=messages, max_tokens=fixed_max_tokens,
                                     temperature=fixed_temperature)

    return FunctionTool(
        name="covenant_inference",
        description=("Buy one inference response from Covenant using the application-configured "
                     "model and USDC spending limits. This tool has one preauthorized purchase "
                     "identity. Repeating the exact task resumes or returns that purchase; "
                     "changing the task is refused. Returns a verified answer with payment "
                     "evidence, or pending. It does not execute the answer or establish its accuracy."),
        func=invoke, input_model=InferenceArguments, result_parser=compact_result,
        approval_mode="always_require" if checkpoint_before_dispatch else "never_require",
    )
