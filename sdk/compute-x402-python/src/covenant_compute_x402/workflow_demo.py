"""Persisted Microsoft Agent Framework incident-review workflow.

All planner calls are made through the application-supplied, budget-controlled
chat client. This module never creates a planner client, wallet or request UUID.
Use a private, trusted state directory: FileCheckpointStorage includes serialized
framework state and is not a format for importing other users' checkpoints.
"""
from __future__ import annotations

import hashlib
import fcntl
from functools import wraps
import json
import os
from pathlib import Path
from typing import Any

from agent_framework import (Agent, Executor, FileCheckpointStorage, WorkflowBuilder,
                             WorkflowContext, handler, response_handler)

from .agent_framework import InferenceArguments, compact_result, covenant_inference_tool

SAMPLE_INCIDENT = (
    "Sample incident report: At 09:10 UTC, checkout errors rose from 0.2% to 8%. "
    "A payment-service deployment completed at 09:05. Database latency remained normal. "
    "The new version retried upstream timeouts three times without backoff. At 09:18, "
    "the on-call engineer rolled back; errors returned to 0.3% by 09:23. No duplicate "
    "charges have been confirmed. Prepare a concise incident review: observed impact, "
    "likely cause with uncertainty, evidence still needed, and recommended follow-up. "
    "Do not claim the investigation is complete or that any follow-up was performed."
)
SYSTEM_POLICY = (
    "You prepare incident reviews for human review. Distinguish supplied observations "
    "from hypotheses. Do not invent evidence, assign confirmed root cause without proof, "
    "or claim you changed systems, contacted people, or completed recommended work."
)
PLANNER_INSTRUCTIONS = (
    "You are an incident-review coordinator. Decide whether the user's task benefits "
    "from the available Covenant inference tool. If using it, provide the complete "
    "incident and requested deliverable as the prompt. You have one configured purchase "
    "slot. Do not invent observations or report actions as performed."
)


def _read(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as stream:
        return json.load(stream)


def _write(path: Path, value: dict[str, Any]) -> None:
    temp = path.with_suffix(".tmp")
    descriptor = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2, allow_nan=False)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temp, path)
    directory = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


class IncidentExecutor(Executor):
    def __init__(self, *, tool: Any, planner: Any):
        super().__init__(id="incident-review")
        self.tool, self.planner = tool, planner

    @handler
    async def plan(self, task: dict, ctx: WorkflowContext) -> None:
        if self.planner is None:
            raise ValueError("planner_required_for_plan")
        agent = Agent(client=self.planner, id="covenant-incident-planner", name="Incident planner",
                      instructions=PLANNER_INSTRUCTIONS, tools=[self.tool],
                      default_options={"tool_choice": "auto", "max_tokens": 1024})
        response = await agent.run(task["task"])
        calls = []
        for message in response.messages:
            for content in message.contents:
                if content.type == "function_approval_request" and content.function_call:
                    calls.append(content.function_call)
        if len(calls) != 1 or calls[0].name != "covenant_inference":
            raise ValueError("planner_must_select_exactly_one_covenant_tool_call")
        call = calls[0]
        arguments = json.loads(call.arguments) if isinstance(call.arguments, str) else dict(call.arguments or {})
        selected = {"name": call.name, "callId": call.call_id,
                    "arguments": InferenceArguments.model_validate(arguments).model_dump(),
                    "task": task["task"], "requestId": task["requestId"],
                    "plannerMessages": [message.to_dict() for message in response.messages]}
        # The official workflow yields IDLE_WITH_PENDING_REQUESTS and saves its
        # checkpoint. No paid client call is possible in this handler.
        await ctx.request_info(selected, str)

    @response_handler
    async def purchase(self, original_request: dict, response: str, ctx: WorkflowContext) -> None:
        if response != "dispatch_saved_purchase":
            raise ValueError("invalid_application_resume_marker")
        arguments = InferenceArguments.model_validate(original_request["arguments"]).model_dump()
        result = await self.tool.invoke(arguments=arguments, tool_call_id=original_request["callId"],
                                        skip_parsing=True)
        await ctx.yield_output({"selection": original_request, "purchase": result})


def _workflow(*, storage: FileCheckpointStorage, tool: Any, planner: Any, workflow_id: str):
    executor = IncidentExecutor(tool=tool, planner=planner)
    return WorkflowBuilder(start_executor=executor, checkpoint_storage=storage,
                           name=workflow_id, max_iterations=4).build()


def _exclusive_run(function):
    @wraps(function)
    async def locked(phase, **kwargs):
        # One process owns the workflow state through its complete phase,
        # including checkpoint writes and uncertain/pending transitions.
        root = Path(kwargs["state_dir"])
        root.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.chmod(root, 0o700)
        descriptor = os.open(root / ".workflow.lock", os.O_RDWR | os.O_CREAT, 0o600)
        try:
            try:
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as exc:
                raise ValueError("workflow_phase_already_running") from exc
            return await function(phase, **kwargs)
        finally:
            os.close(descriptor)
    return locked


@_exclusive_run
async def run_demo(
    phase: str, *, client: Any, state_dir: str | Path, request_id: str,
    workflow_id: str, model: str, planner: Any = None, max_tokens: int = 2048,
    temperature: float = 0.2, system_prompt: str = SYSTEM_POLICY,
    task: str = SAMPLE_INCIDENT,
) -> dict[str, Any]:
    """Run exactly one explicit plan, purchase, replay or finish phase.

    ``plan`` allows one planner turn, persists its selected call and returns
    before payment. ``purchase`` resumes that exact checkpoint and may return
    pending; repeat it to reconcile that same identity. ``replay`` is allowed
    only after completion and runs the identical stored tool call. ``finish``
    sends the already-verified result to the planner without any paid tool.
    Planner payment and limits remain the calling application's responsibility.
    """
    if phase not in {"plan", "purchase", "replay", "finish"}:
        raise ValueError("invalid_demo_phase")
    if not isinstance(workflow_id, str) or not workflow_id or len(workflow_id) > 160:
        raise ValueError("trusted_workflow_id_required")
    root = Path(state_dir)
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(root, 0o700)
    manifest_path = root / "workflow.json"
    identity = {"requestId": request_id, "workflowId": workflow_id, "model": model,
                "maxTokens": max_tokens, "temperature": temperature,
                "systemPrompt": system_prompt, "task": task}
    tool = covenant_inference_tool(client=client, request_id=request_id, model=model,
                                   max_tokens=max_tokens, temperature=temperature,
                                   system_prompt=system_prompt, checkpoint_before_dispatch=True)
    checkpoint_dir = root / "checkpoints"
    checkpoint_dir.mkdir(exist_ok=True, mode=0o700)
    os.chmod(checkpoint_dir, 0o700)
    storage = FileCheckpointStorage(checkpoint_dir)
    workflow = _workflow(storage=storage, tool=tool, planner=planner, workflow_id=workflow_id)
    state = _read(manifest_path) if manifest_path.exists() else None
    if state is not None and state["identity"] != identity:
        raise ValueError("saved_workflow_identity_mismatch")

    if phase == "plan":
        if state is not None:
            if state.get("selection"):
                return {"phase": "plan", "cached": True, **state}
            # An interrupted planning call may already have been billed. Never
            # silently send another one under a newly-created run.
            raise ValueError("planning_outcome_uncertain_inspect_saved_checkpoints")
        state = {"version": 1, "identity": identity, "status": "planning"}
        # Exclusive creation prevents competing planners for the same run.
        descriptor = os.open(manifest_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(state, stream)
            stream.flush()
            os.fsync(stream.fileno())
        result = await workflow.run({"task": task, "requestId": request_id})
        pending = result.get_request_info_events()
        if len(pending) != 1:
            raise ValueError("saved_purchase_request_missing")
        checkpoint = await storage.get_latest(workflow_name=workflow_id)
        if checkpoint is None:
            raise ValueError("checkpoint_missing_before_purchase")
        state.update(status="planned", checkpointId=checkpoint.checkpoint_id,
                     requestEventId=pending[0].request_id, selection=pending[0].data)
        _write(manifest_path, state)
        return {"phase": phase, "cached": False, **state}

    if state is None or not state.get("selection"):
        raise ValueError("saved_plan_required")
    completed_path = root / "completed.json"
    if phase in {"replay", "finish"} and (state["status"] != "completed" or not completed_path.exists()):
        raise ValueError("verified_completed_purchase_required")
    if phase == "finish":
        if state.get("finish"):
            return {"phase": phase, "cached": True, "summary": state["finish"]}
        if state.get("finishStarted"):
            raise ValueError("finish_outcome_uncertain_no_automatic_retry")
        if planner is None:
            raise ValueError("planner_required_for_finish")
        completed = _read(completed_path)
        if completed["status"] != "completed" or completed["requestId"] != request_id:
            raise ValueError("completed_purchase_identity_mismatch")
        state["finishStarted"] = True
        _write(manifest_path, state)
        agent = Agent(client=planner, id="covenant-incident-reviewer", name="Incident reviewer",
                      instructions="Summarize the supplied incident review in two sentences for a human reviewer. "
                                   "Keep uncertainties and distinguish recommendations from completed actions.",
                      default_options={"max_tokens": 512})
        response = await agent.run(compact_result(completed))
        state["finish"] = response.text
        _write(manifest_path, state)
        return {"phase": phase, "cached": False, "summary": response.text}

    # Resuming the original waiting checkpoint recovers the exact original
    # selected arguments. It does not run the planner again.
    result = await workflow.run(checkpoint_id=state["checkpointId"], checkpoint_storage=storage,
                                responses={state["requestEventId"]: "dispatch_saved_purchase"})
    outputs = result.get_outputs()
    if len(outputs) != 1:
        raise ValueError("purchase_output_missing")
    output = outputs[0]
    if output["selection"] != state["selection"] or output["purchase"]["requestId"] != request_id:
        raise ValueError("saved_selection_or_purchase_mismatch")
    purchase = output["purchase"]
    if phase == "replay":
        if purchase != _read(completed_path):
            raise ValueError("completed_replay_changed")
    elif purchase["status"] == "completed":
        _write(completed_path, purchase)
        state["status"] = "completed"
        state["textSha256"] = hashlib.sha256(purchase["text"].encode()).hexdigest()
        _write(manifest_path, state)
    else:
        state["status"] = "pending"
        _write(manifest_path, state)
    return {"phase": phase, **output}
