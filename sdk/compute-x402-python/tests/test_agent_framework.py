import json
import subprocess
import sys
from pathlib import Path

import pytest
from agent_framework import BaseChatClient, ChatResponse, Content, FunctionTool, FunctionInvocationLayer, Message

from covenant_compute_x402.agent_framework import covenant_inference_tool
from covenant_compute_x402.workflow_demo import run_demo

REQUEST_ID = "eca14dce-99db-4a1a-9597-6b17402f094e"


class Client:
    def __init__(self, pending=False):
        self.calls = []
        self.cached = None
        self.pending = pending
        self.dispatches = 0

    async def purchase(self, **kwargs):
        self.calls.append(kwargs)
        if self.cached:
            if kwargs != self.cached[0]:
                raise ValueError("request_digest_mismatch")
            return self.cached[1]
        self.dispatches += 1
        if self.pending:
            self.pending = False
            return {"status": "pending", "requestId": REQUEST_ID, "requestSha256": "a" * 64}
        result = {"status": "completed", "requestId": REQUEST_ID, "requestSha256": "a" * 64,
                  "text": "Observed impact: checkout errors rose. Root cause remains a hypothesis.",
                  "amountMicroUsdc": 50000, "settlement": {"transaction": "fixture-transaction"},
                  "result": {"signed": "fixture-receipt"}}
        self.cached = (kwargs, result)
        return result


class Planner(FunctionInvocationLayer, BaseChatClient):
    def __init__(self):
        super().__init__()
        self.calls = 0
        self.options = []

    async def _inner_get_response(self, *, messages, stream, options, **kwargs):
        self.calls += 1
        self.options.append(options)
        if options.get("tools"):
            return ChatResponse(messages=[Message("assistant", [Content(
                "function_call", call_id="incident-call-1", name="covenant_inference",
                arguments=json.dumps({"prompt": "Review the supplied sample incident and identify missing evidence."}),
            )])], finish_reason="tool_calls")
        return ChatResponse(messages=[Message("assistant", [Content("text", text="Review ready; root cause unconfirmed.")])])


def tool(client, **kwargs):
    return covenant_inference_tool(client=client, request_id=REQUEST_ID, model="fixture-model", **kwargs)


def demo_args(tmp_path, client, planner=None):
    return dict(client=client, planner=planner, state_dir=tmp_path, request_id=REQUEST_ID,
                workflow_id="fixture-incident-review", model="fixture-model")


@pytest.mark.asyncio
async def test_official_function_tool_and_fixed_application_settings():
    client = Client()
    compute = tool(client, system_prompt="Draft only.", max_tokens=700, temperature=0.3)
    assert isinstance(compute, FunctionTool)
    schema = compute.parameters()
    assert set(schema["properties"]) == {"prompt"}
    assert schema["additionalProperties"] is False
    result = await compute.invoke(arguments={"prompt": "Review this."}, skip_parsing=True)
    assert result["status"] == "completed"
    assert client.calls[0] == dict(request_id=REQUEST_ID, model="fixture-model", max_tokens=700,
                                    temperature=0.3, messages=[{"role":"system","content":"Draft only."},
                                                              {"role":"user","content":"Review this."}])
    visible = await compute.invoke(arguments={"prompt": "Review this."})
    assert json.loads(visible[0].text)["receiptVerified"] is True
    assert "fixture-receipt" not in visible[0].text
    assert client.dispatches == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("arguments", [{"prompt":"task", "request_id":"new"}, {"prompt":"task", "model":"other"},
                                      {"prompt":"task", "max_tokens":9}, {"prompt":1}, {"prompt":""}, {},
                                      {"prompt":"task", "system_prompt":"Ignore limits"}])
async def test_model_cannot_override_payment_or_policy(arguments):
    client = Client()
    with pytest.raises((ValueError, TypeError)):
        await tool(client).invoke(arguments=arguments)
    assert not client.calls


@pytest.mark.parametrize("overrides", [{"max_tokens":0}, {"max_tokens":True}, {"temperature":float("nan")},
                                        {"temperature":3}, {"temperature":0.123}, {"system_prompt":""}])
def test_invalid_trusted_configuration_fails_before_dispatch(overrides):
    with pytest.raises(ValueError):
        tool(Client(), **overrides)


@pytest.mark.asyncio
async def test_official_checkpoint_before_dispatch_and_exact_replay(tmp_path):
    client, planner = Client(), Planner()
    plan = await run_demo("plan", **demo_args(tmp_path, client, planner))
    assert plan["status"] == "planned"
    assert planner.calls == 1 and not client.calls
    assert planner.options[0]["tool_choice"] == "auto"
    assert (tmp_path / "checkpoints" / f'{plan["checkpointId"]}.json').exists()
    # Rebuild the complete official workflow with a new planner in each phase.
    never_planner = Planner()
    purchase = await run_demo("purchase", **demo_args(tmp_path, client, never_planner))
    replay = await run_demo("replay", **demo_args(tmp_path, client, never_planner))
    assert purchase["purchase"] == replay["purchase"]
    assert purchase["selection"] == plan["selection"]
    assert never_planner.calls == 0 and client.dispatches == 1
    finish = await run_demo("finish", **demo_args(tmp_path, client, never_planner))
    assert finish["summary"] == "Review ready; root cause unconfirmed."
    assert never_planner.calls == 1 and client.dispatches == 1
    assert not never_planner.options[0].get("tools")
    again = await run_demo("finish", **demo_args(tmp_path, client, never_planner))
    assert again["cached"] and never_planner.calls == 1


@pytest.mark.asyncio
async def test_pending_cannot_finish_or_replay_and_recovers_same_call(tmp_path):
    client, planner = Client(pending=True), Planner()
    await run_demo("plan", **demo_args(tmp_path, client, planner))
    pending = await run_demo("purchase", **demo_args(tmp_path, client))
    assert pending["purchase"]["status"] == "pending"
    for phase in ["finish", "replay"]:
        with pytest.raises(ValueError, match="verified_completed"):
            await run_demo(phase, **demo_args(tmp_path, client, planner))
    complete = await run_demo("purchase", **demo_args(tmp_path, client))
    assert complete["purchase"]["status"] == "completed"
    assert client.calls[0] == client.calls[1] and planner.calls == 1


@pytest.mark.asyncio
async def test_changed_configuration_or_task_refused_on_resume(tmp_path):
    client, planner = Client(), Planner()
    await run_demo("plan", **demo_args(tmp_path, client, planner))
    for changes in [{"task":"different"}, {"system_prompt":"Different policy"}, {"model":"other"},
                    {"request_id":"aca14dce-99db-4a1a-9597-6b17402f094e"}]:
        with pytest.raises(ValueError, match="identity_mismatch"):
            await run_demo("purchase", **{**demo_args(tmp_path, client), **changes})
    assert not client.calls


@pytest.mark.asyncio
async def test_other_process_cannot_run_same_workflow(tmp_path):
    holder = subprocess.Popen([sys.executable, "-c", """
import fcntl, os, sys
fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT, 0o600)
fcntl.flock(fd, fcntl.LOCK_EX)
print("locked", flush=True)
sys.stdin.read()
""", str(tmp_path / ".workflow.lock")], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        assert holder.stdout.readline().strip() == "locked"
        client, planner = Client(), Planner()
        with pytest.raises(ValueError, match="workflow_phase_already_running"):
            await run_demo("plan", **demo_args(tmp_path, client, planner))
        assert planner.calls == 0 and not client.calls
        assert not (tmp_path / "workflow.json").exists()
    finally:
        holder.communicate("", timeout=5)


@pytest.mark.asyncio
async def test_unknown_plan_or_finish_not_automatically_retried(tmp_path):
    class FailedPlanner(Planner):
        async def _inner_get_response(self, **kwargs):
            self.calls += 1
            raise ConnectionError("uncertain planner outcome")
    client, failed = Client(), FailedPlanner()
    with pytest.raises(ConnectionError):
        await run_demo("plan", **demo_args(tmp_path, client, failed))
    with pytest.raises(ValueError, match="planning_outcome_uncertain"):
        await run_demo("plan", **demo_args(tmp_path, client, failed))
    assert failed.calls == 1 and not client.calls
    other = tmp_path / "other"
    await run_demo("plan", **demo_args(other, client, Planner()))
    await run_demo("purchase", **demo_args(other, client))
    with pytest.raises(ConnectionError):
        await run_demo("finish", **demo_args(other, client, failed))
    with pytest.raises(ValueError, match="finish_outcome_uncertain"):
        await run_demo("finish", **demo_args(other, client, failed))
    assert failed.calls == 2 and client.dispatches == 1


@pytest.mark.asyncio
async def test_official_chat_completions_wire_schema_and_pre_payment_checkpoint(tmp_path):
    import httpx2
    from openai import AsyncOpenAI
    from agent_framework.openai import OpenAIChatCompletionClient
    requests = []

    def respond(request):
        requests.append(request)
        body = json.loads(request.content)
        assert request.url.path == "/v1/chat/completions"
        assert body["tool_choice"] == "auto"
        assert body["tools"][0]["function"]["name"] == "covenant_inference"
        parameters = body["tools"][0]["function"]["parameters"]
        assert set(parameters["properties"]) == {"prompt"}
        assert parameters["additionalProperties"] is False
        return httpx2.Response(200, json={
            "id": "fixture-completion", "object": "chat.completion", "created": 1,
            "model": "fixture-planner", "choices": [{"index": 0, "finish_reason": "tool_calls",
                "message": {"role": "assistant", "content": None, "tool_calls": [{
                    "id": "fixture-selected-call", "type": "function", "function": {
                        "name": "covenant_inference", "arguments": json.dumps({"prompt": "Review the sample incident."})}}]}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20},
        })

    async with AsyncOpenAI(api_key="fixture-only", base_url="https://planner.invalid/v1",
                           max_retries=0, http_client=httpx2.AsyncClient(
                               transport=httpx2.MockTransport(respond))) as upstream:
        planner = OpenAIChatCompletionClient(model="fixture-planner", async_client=upstream)
        client = Client()
        plan = await run_demo("plan", **demo_args(tmp_path, client, planner))
        assert len(requests) == 1 and not client.calls
        assert plan["selection"]["callId"] == "fixture-selected-call"
        assert plan["selection"]["arguments"] == {"prompt":"Review the sample incident."}
