# covenant-compute-buyer

Buyer-side client for the Covenant compute network: sign a job
envelope, dispatch it to a coordinator, poll the receipt, and re-verify
every operator commitment locally — the relay is never trusted with
the verdict. Ships `covenant-compute-mcp`, a stdio MCP server, so any
MCP client can buy compute with zero Covenant plumbing.
The network-level map lives at
[docs/compute-network.md](../../../docs/compute-network.md).

## As an MCP server

```jsonc
// e.g. an MCP client's server config
{
  "command": "covenant-compute-mcp",
  "env": {
    "COVENANT_COMPUTE_COORDINATOR_URL": "http://127.0.0.1:8720",
    "COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC": "100000",   // per-call ceiling, default $1
    "COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC": "1000000",  // optional session cap
    "COVENANT_COMPUTE_RPC_URL": "https://api.devnet.solana.com" // optional, for compute.verify
  }
}
```

Seventeen tools:

| Tool | What it buys or shows |
| --- | --- |
| `compute.infer` | one inference call (prompt or chat messages, optional base64 `images` for a vision model, plus optional sampling knobs: `temperature`, `top_p`, `max_tokens`, `seed`, `presence_penalty`, `frequency_penalty`, `stop`, `logprobs` for per-token probabilities, and `response_format` for JSON or schema-constrained output) |
| `compute.embed` | one embedding vector from `text`, or a `texts` batch bought as one job (one vector per input, in order) |
| `compute.transcribe` | one speech-to-text transcription of a base64 `audio_base64` clip; set `language` to skip detection, `translate` to return English, or `timestamps` for per-segment start/end times |
| `compute.speak` | one text-to-speech clip from `text`; set `voice`, `format` (wav or aiff), or `speed`. The clip saves under the server's `clips/` and the result names the file, so the base64 audio never lands in the agent's context |
| `compute.run` | one batch command — it executes on a stranger's machine, and the tool description says so |
| `compute.stream_start` | `compute.infer`, returning the job id immediately so the output can be read as it generates |
| `compute.stream_poll` | cursor-read a streaming job's live chunks; the concluding poll carries the verified output and receipt |
| `compute.receipts` | your verified job history; unpaid rows name their `refund_reason` |
| `compute.output` | re-read a past job's output and its locally re-verified receipt, so an answer bought once survives a lost session |
| `compute.balance` | deposited / charged / withdrawn / available funds |
| `compute.capacity` | the live directory: what (kind, model) rows are purchasable right now, operator counts, and the ask range a price must reach to match |
| `compute.deposit` | claim an on-chain deposit against this deployment's rail |
| `compute.withdraw` | move unspent balance back out to a wallet you name |
| `compute.withdrawals` | your withdrawal history: amount, recipient, memo, and whether each transfer has landed |
| `compute.dispute` | file a signed dispute against a concluded job |
| `compute.cancel` | withdraw a job no operator has accepted yet — full refund now, and its idempotency key is freed to buy again |
| `compute.verify` | hold the chain to one job's money trail, via your own RPC endpoint |

The streaming pair buys exactly what `compute.infer` buys — the live
feed is an unsigned preview, the operator's signed receipt over the
final output is the artifact, and the session's spend is committed only
when that receipt verifies (a failed or refunded job never charges).
`COVENANT_COMPUTE_MAX_ACTIVE_STREAMS` (default 4) caps how many
streaming jobs may run at once; their offers count against the session
cap while they run. A call that names no deadline gets
`COVENANT_COMPUTE_DEADLINE_MS` (default 60000).

`compute.infer`, `compute.embed`, `compute.transcribe`, `compute.speak`,
and `compute.run` take an optional `idempotency_key` that makes the purchase
exactly-once: the signed job envelope journals
in the server home (`purchases.jsonl`) before its first submission, so
repeats of the key — a same-session retry or one after a crash —
re-drive the same job and return the first call's verified result
instead of buying a second one. A key only frees for re-buying when
its purchase provably concluded unpaid: the job refunded, rejected, or
failed, or the coordinator refused the submission with a verdict on
the envelope itself. A funding refusal keeps it bound — top up and
retry the same key. A key names one purchase, so an explicit argument
that contradicts it (a different prompt, price, model, or deadline) is
refused rather than silently answered with the old job; use a fresh
key for new work.

Every purchase returns the job output plus a metadata block carrying
the operator's signed receipt, already re-verified client-side
(signature, assigned operator key, output hash), and — once the
coordinator's payout push lands — the on-chain pointer for it: amount,
transaction signature, and the `compute-payout:v1:<job_id>:<receipt
signature>` memo stamped on the transfer. Both spend caps refuse
before anything is signed or dispatched. The buyer identity persists
under `$COVENANT_COMPUTE_MCP_HOME` (default `~/.covenant-compute-mcp`);
reads that are private to that identity (history, balance) go out as
signed reads. `COVENANT_COMPUTE_REFERRAL_CODE` (optional) rides each
dispatch so the partner who onboarded this buyer earns their disclosed
share of the marketplace fee.

## As an OpenAI-compatible endpoint

`covenant-compute-openai` is an HTTP server that speaks OpenAI's
`POST /v1/responses` and `POST /v1/chat/completions`, the legacy
`POST /v1/completions`, `POST /v1/embeddings`,
`POST /v1/audio/transcriptions`, `POST /v1/audio/translations`, and
`POST /v1/audio/speech`. Point any OpenAI client at its base URL and every
response, completion, embedding, transcription, translation, or spoken clip
is bought on the compute network, paid, and returned in OpenAI's own
response shape, with no client code changes.

```sh
export COVENANT_COMPUTE_COORDINATOR_URL=http://127.0.0.1:8720
export COVENANT_COMPUTE_OPENAI_API_KEY=sk-your-key   # optional; gates /v1/*
covenant-compute-openai                              # serves 127.0.0.1:8787
```

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:8787/v1", api_key="sk-your-key")
client.chat.completions.create(
    model="qwen2.5:0.5b",
    messages=[{"role": "user", "content": "hello"}],
)
client.embeddings.create(
    model="nomic-embed-text",
    input=["first text", "second text"],
)
```

`GET /v1/models` lists the chat, embedding, and transcription models the
network is serving right now, and `GET /v1/models/{model}` retrieves one (a
404 in OpenAI's error shape when it is not being served). `stream: true` returns
the tokens as server-sent
`chat.completion.chunk` frames followed by `[DONE]`; add
`stream_options: {include_usage: true}` for a final frame carrying the
receipt's token counts. A completion's `finish_reason` is `stop`,
`length` when the model was cut off at the `max_tokens` limit, or
`tool_calls` when it stopped to call a tool, so a client can tell a
finished answer from a truncated one from a tool request. Set `logprobs:
true` (with an optional `top_logprobs` count, `0..=20`) to get each token's
log probability back in `choices[].logprobs`, the standard OpenAI shape;
the probabilities ride the signed receipt's attested output. Ask for
several completions at once with `n` (up to 8): the network runs that many
independent paid jobs, so `choices` returns that many entries, `usage` sums
across them, and `covenant.receipts` carries one verified receipt per choice.
The whole batch is reserved against your caps before any job is placed, so a
fan-out your session cap can't cover is refused without charging for part of
it. Streaming serves one completion per request, so pair `stream: true` with
`n: 1`. An output-changing knob the network can't honor is refused before you
are charged rather than silently ignored: `logit_bias`, and
`parallel_tool_calls: false` when tools are in play. `POST /v1/embeddings`
takes a string or an array of strings and returns one vector each, in
`float` or `base64` form (the encoding the OpenAI SDKs request by
default); the served model fixes the vector width, so a `dimensions`
reshape is refused. Chat `content` may be a plain string or OpenAI's array
of `{type: "text", text}` parts, and the `developer` role folds to a system
instruction. The legacy `POST /v1/completions` takes a raw `prompt` string
instead of `messages` and returns a `text_completion`; it shares every
sampling knob and the same caps and receipt, streams `text_completion`
frames, and completes one prompt per call (a multi-prompt batch, token-id
inputs, and `logprobs`/`logit_bias`/`echo`/`suffix` are refused rather
than silently dropped). Each route takes the `model` field as the routing key,
so a batch or lease job (which has no model to route by) stays on the CLI
and MCP surfaces. Price defaults to the cheapest matching operator's ask under
`COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC`, and
`COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC` caps one server run's total spend
(it resets on restart, so the funded balance is the lasting limit). Every
response carries OpenAI's own shape plus a `covenant` object with the
operator's signed, locally re-verified receipt (job id, price, operator,
result hash), or a `covenant.receipts` array with one receipt per choice when
you ask for several: a plain OpenAI client ignores it, a Covenant-aware one
holds the job to its on-chain money trail. The buyer identity persists
under `COVENANT_COMPUTE_OPENAI_HOME` and funds the same way as any other
buyer. Set an `x-covenant-min-reputation-bps` header (basis points,
`8000` = 80%) to confine the completion to operators rated at or above it.
An underfunded buyer gets a 402, a run over its session cap a 429, and a
request no operator can serve a 502. A `SIGTERM` drains the requests already
in flight before the process exits, so a restart returns the completions you
are being charged for rather than dropping them.

### The Responses API

`POST /v1/responses` serves OpenAI's newer Responses surface, the one
`client.responses.create(...)` and the OpenAI Agents SDK call. Point the
same base URL at it and the request is bought, paid, and returned as a
standard `response` object whose `output` array carries the assistant's
`output_text`, alongside the same `covenant` receipt every other route
returns.

```python
client.responses.create(
    model="qwen2.5:0.5b",
    instructions="be terse",
    input="hello",
)
```

`input` is either a string (one user turn) or an array of typed input
items, and a top-level `instructions` string leads as the system turn.
`max_output_tokens` caps the answer, and a generation stopped at that limit
returns `status: "incomplete"` with `incomplete_details.reason` set to
`"max_output_tokens"`, so a client can tell a short answer from a truncated
one. `stream: true` returns the typed Responses event sequence
(`response.created`, the message item and its text part, then
`response.output_text.delta` frames) closed by `response.completed`, or
`response.incomplete` at the token cap, the events the OpenAI Agents SDK
reads. Set `text.format` to a `json_schema` (the Agents SDK's structured
`output_type`) or `json_object` and the constraint travels into the signed,
paid envelope, the same structured-output path `/v1/chat/completions` takes.
`tools` and `tool_choice` open the OpenAI Agents SDK's tool loop. Offer
functions the Responses way, with each function's `name`, `description`, and
`parameters` at the top level, and the model can answer with `function_call`
output items instead of prose. Your agent runs them and feeds each result
back as a `function_call_output` item on the next turn, so the operator runs
the real conversation. Streaming carries the calls too: each `function_call`
item opens, its arguments arrive, and it closes before `response.completed`.
A hosted tool such as web search runs inside OpenAI's own service, so the
network refuses it rather than forwarding a call it cannot make. Vision
travels too: an `input_image` content part carries its bytes inline as a
base64 `data:` URI to the operator's vision model, and a remote URL is
refused because the network never fetches a buyer's image. `reasoning`,
`background`, and `previous_response_id` are each refused with a clear
message before any spend rather than silently dropped. The network stores no
response, so `store` reads back `false` and a follow-up turn resends the
whole conversation as `input`.

### Tool calling

Pass `tools` and an optional `tool_choice` and the model can answer with
function calls instead of prose. The reply comes back in OpenAI's shape:
`message.tool_calls` carries each call's `id`, function name, and JSON
`arguments`, `content` is `null`, and `finish_reason` is `tool_calls`. A
streamed request delivers the same calls in a final `chat.completion.chunk`.
Run the functions, then send the results back as `tool` messages to
continue the turn.

```python
tools = [{
    "type": "function",
    "function": {
        "name": "get_weather",
        "parameters": {"type": "object", "properties": {"city": {"type": "string"}}},
    },
}]
first = client.chat.completions.create(
    model="qwen2.5:0.5b",
    messages=[{"role": "user", "content": "weather in Paris?"}],
    tools=tools,
)
call = first.choices[0].message.tool_calls[0]
client.chat.completions.create(
    model="qwen2.5:0.5b",
    tools=tools,
    messages=[
        {"role": "user", "content": "weather in Paris?"},
        first.choices[0].message,
        {"role": "tool", "tool_call_id": call.id, "content": "18C and clear"},
    ],
)
```

The tool calls are part of the operator's signed receipt, so a
Covenant-aware client verifies what the model asked for, not just what it
said. Whether a given model calls tools depends on the model an operator
serves; a model that ignores the offer just answers in prose.

### Structured output

Set `response_format` to constrain the reply. `{"type": "json_object"}`
asks for any valid JSON; `{"type": "json_schema", "json_schema": {"name":
..., "schema": {...}}}` holds the output to a JSON schema. The constraint
rides the operator's signed input, so it is part of what the receipt
attests.

```python
client.chat.completions.create(
    model="qwen2.5:0.5b",
    messages=[{"role": "user", "content": "the capital of France, as JSON"}],
    response_format={
        "type": "json_schema",
        "json_schema": {
            "name": "capital",
            "schema": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
            },
        },
    },
)
```

Whether the output truly conforms depends on the model an operator serves;
the network passes the constraint to the backend's own structured-output
control (an Ollama node's `format`, an OpenAI-compatible node's
`response_format`).

### Images

Attach an image and an operator serving a vision model can describe it,
read its text, or answer questions about it. Send it as an inline base64
data URI in an `image_url` content part, the shape the OpenAI SDKs already
produce:

```python
with open("chart.png", "rb") as f:
    b64 = base64.b64encode(f.read()).decode()

client.chat.completions.create(
    model="moondream",
    messages=[{
        "role": "user",
        "content": [
            {"type": "text", "text": "What does this chart show?"},
            {"type": "image_url", "image_url": {"url": f"data:image/png;base64,{b64}"}},
        ],
    }],
)
```

The image travels inside the operator's signed input, so it is part of what
the receipt attests. Only inline data URIs are served: the network relays
the bytes you send and never fetches a remote URL, so a remote `image_url`
is refused up front. The answer depends on the vision model an operator
serves.

### Audio transcription

`POST /v1/audio/transcriptions` turns speech into text. Upload an audio
file as multipart form data, the shape the OpenAI SDKs already send, and an
operator serving a speech model returns the transcript:

```python
client.audio.transcriptions.create(
    model="whisper-1",
    file=open("meeting.wav", "rb"),
)
```

A 16 kHz mono WAV reads on every operator; other containers depend on the
backend an operator runs. `response_format` is `json` (the default,
`{"text": …}` plus the `covenant` receipt) or `text` (the bare transcript).
For captions, `verbose_json` returns the transcript with a `segments` array
of `{start, end, text}` timings, and `srt` or `vtt` return ready-to-use
subtitles:

```python
client.audio.transcriptions.create(
    model="whisper-1",
    file=open("meeting.wav", "rb"),
    response_format="verbose_json",
)
```

`language` takes an ISO code to skip auto-detection. The audio rides inside
the operator's signed input, so the clip a buyer paid to have transcribed
is part of what the receipt attests.

`POST /v1/audio/translations` takes the same upload and returns the
speech in English whatever language it was spoken in, so it takes no
`language` field:

```python
client.audio.translations.create(
    model="whisper-1",
    file=open("entrevista.wav", "rb"),
)
```

### Audio speech

`POST /v1/audio/speech` turns text into audio, the mirror of transcription.
An operator running a synthesis backend returns the clip, and the response
body is the audio itself, exactly where an OpenAI client expects the bytes:

```python
speech = client.audio.speech.create(
    model="say-1",
    voice="alloy",
    input="Covenant Compute speaks.",
    response_format="wav",
)
speech.write_to_file("hello.wav")
```

The network produces `wav` (the default) and `aiff`; a request for another
container is refused before it costs anything, rather than answered with the
wrong bytes. A stock OpenAI voice is spoken in the operator's default voice;
name a voice the backend serves to pick your own, and `speed` (0.25 to 4.0)
sets the pace. The verified receipt rides an `x-covenant-receipt` response
header so the body stays pure audio.

## As an Anthropic-compatible endpoint

The same server also speaks Anthropic's `POST /v1/messages`. Point an
Anthropic client at its base URL, send your key as an `x-api-key` header (a
bearer token works too), and every message is bought on the compute network,
paid, and returned in Anthropic's own response shape, with no client code
changes.

```python
from anthropic import Anthropic

client = Anthropic(base_url="http://127.0.0.1:8787", api_key="sk-your-key")
client.messages.create(
    model="qwen2.5:0.5b",
    max_tokens=256,
    system="You are terse.",
    messages=[{"role": "user", "content": "hello"}],
)
```

`max_tokens` is required, the way Anthropic requires it. A system prompt,
multi-turn `messages`, `temperature`, `top_p`, and `stop_sequences` all carry
through. A message's `content` is a plain string or Anthropic's array of
blocks: `text` blocks join into the prompt, and an `image` block with a base64
`source` reaches an operator serving a vision model. The reply is a standard
`message` with a `content` array, a `stop_reason` (`end_turn`, `max_tokens`,
or `tool_use`), and `usage` token counts, plus one extra `covenant` field with
the operator's signed, locally re-verified receipt. A plain Anthropic client
ignores that field; a Covenant-aware one holds the job to its on-chain money
trail.

Tools work the whole way through. Offer functions with `tools` (Anthropic's
`{name, description, input_schema}` shape) and an optional `tool_choice`
(`auto`, `any`, `tool`, or `none`), and the model can answer with `tool_use`
blocks carrying each call's `id`, name, and parsed `input`. The turn stops for
`tool_use`; run the functions and feed the results back as `tool_result`
blocks on the next user turn to continue.

`stream: true` returns Anthropic's server-sent event sequence (`message_start`,
`content_block_start`, `content_block_delta`, `content_block_stop`,
`message_delta`, `message_stop`), with the verified receipt on the closing
`message_delta`. The token counts arrive there too, since the network meters a
job as it completes.

`client.models.list()` and `client.models.retrieve(id)` report the chat models
the network is serving right now, in Anthropic's Models API shape, so a client
or framework that discovers models before it calls them reads the live catalog.
Only models a message can use are listed; a name no operator serves is a 404 in
Anthropic's error shape. The network does not publish a display name or release
date per model, so each entry carries the model id as its `display_name` and the
Unix epoch as its `created_at`.

The message route shares what the chat route uses: routing on `model`, default
pricing under your per-call and session caps, and the same buyer identity and
receipt. A control the network can't honor is refused before you are charged
rather than silently dropped, including `top_k` and `disable_parallel_tool_use`
when tools are in play. An underfunded buyer gets a 429, and a request no
operator can serve a 502, in Anthropic's error shape.

Prompt-caching `cache_control` blocks are accepted and ignored. The network has
no cache tier, so a repeated prefix is priced in full and the response `usage`
carries no `cache_read_input_tokens` or `cache_creation_input_tokens`, which is
how a client can confirm nothing was cached.

## As a human CLI

`covenant-compute` is the same buyer for a person at a terminal. It
loads the same identity as the MCP server (under
`$COVENANT_COMPUTE_MCP_HOME`), so a wallet funded from the CLI buys jobs
through the MCP server and the other way round.

```sh
export COVENANT_COMPUTE_COORDINATOR_URL=http://127.0.0.1:8720

covenant-compute whoami                          # this buyer's pubkey and home
covenant-compute capacity                        # what is purchasable right now
covenant-compute deposit <tx-signature>          # claim an on-chain top-up
covenant-compute balance                         # funds and how to top up
covenant-compute infer "your prompt"             # buy one inference call
covenant-compute embed "your text"               # buy one embedding vector
covenant-compute transcribe --audio clip.wav     # buy one speech-to-text transcription
covenant-compute speak "your text"               # buy one text-to-speech clip
covenant-compute run "your command"              # buy one batch command
covenant-compute receipts                        # your verified job history
covenant-compute output <job-id>                 # re-read a past job's verified output
covenant-compute verify <job-id>                 # hold the chain to a payout
covenant-compute withdraw <micro-usdc> <wallet>  # move unspent balance out
covenant-compute withdrawals                     # your withdrawal history
covenant-compute dispute <job-id> "reason"       # file a signed dispute
covenant-compute cancel <job-id>                 # refund a job no operator took
covenant-compute lease open --minutes 30 --rate 200 --ssh-key ~/.ssh/id_ed25519.pub  # rent a GPU
covenant-compute lease view <job-id>             # a live lease's endpoint, meter and cost
covenant-compute lease close <job-id>            # end a running lease and settle the meter
covenant-compute vault put deploy-token "sk-…"   # store a secret, sealed under a key only you hold
covenant-compute vault get deploy-token          # fetch and open a secret (raw bytes to stdout)
covenant-compute vault ls                        # your stored secrets, and which you can open here
covenant-compute vault rm deploy-token           # delete a secret and drop its local key
covenant-compute vault key export deploy-token   # print a secret's key, to back it up or move it
covenant-compute vault key import deploy-token <key>  # save a key exported from another machine
```

Every read prints a human summary; add `--json` for the underlying
record. A buy is capped by `COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC` and
refused before anything is signed; `--price` sets the offer under that
ceiling, and `--model` pins an operator serving a given model.
`--gpu-class` (`rtx-4090`, `h100`, or `cpu`) and `--min-vram-gb` require
specific hardware; the requestable classes are what `capacity` lists,
and they matter most for `run`, which has no model to route by.
`--min-reputation-bps` (basis points, `8000` = 80%) confines the job to
operators rated at or above it; a job no operator clears refunds rather
than run on a lesser one.
`--deadline-ms` bounds the job. On `infer`, `--system` sends a system
prompt ahead of yours, `--image <path>` attaches a local image to your
prompt for a vision model and repeats for several, `--messages-file <path>`
sends a full multi-turn conversation as JSON `[{"role","content"},…]` (`-`
reads stdin, and it replaces the prompt rather than adding to it), and
`--temperature`,
`--top-p`, `--max-tokens`, `--seed`, `--presence-penalty`,
`--frequency-penalty` and `--stop` set sampling; each is
validated locally and refused before anything is signed. `--response-format
json` asks for any valid JSON, or pass a path to a JSON schema file for
schema-constrained output. `--logprobs <n>` returns the log probability of
each generated token with `n` most-likely alternatives (`0..=20`), carried
in the signed receipt's attested output. `--tools <path>` offers the model
a JSON file of function definitions (OpenAI's shape), and `--tool-choice`
sets whether it may call one (`auto`), must not (`none`), must (`required`),
or must call one function by name; a reply that calls a tool carries the
calls in the signed receipt. `infer --stream` prints tokens as
they generate. `transcribe` takes `--audio <path>` (the clip to
transcribe), with `--language <code>` to name the spoken language and
`--translate` to return the speech in English instead. `speak` runs the
other way: it turns its text into a clip, written beside you as
`speech-<job-id>.wav`, with `--voice <name>` to pick a voice the operator
serves, `--format wav|aiff` for the container (which sets the extension),
and `--speed <n>` (0.25 to 4.0) for the pace. `--idempotency-key
<key>` makes any buy exactly-once: the signed order is journaled locally before it is
sent, so a retry under the same key replays the recorded answer instead
of paying twice, and reusing a key with different arguments is refused
rather than answered with the stale job. It can't combine with
`--stream`, which returns before the answer is verified. `--dry-run`
previews any buy: it resolves the price,
matched hardware, and deadline a real buy would use, prints them, and
spends nothing, so a buyer can confirm the cost before committing. It
buys nothing, so it pairs with neither `--stream` nor
`--idempotency-key`. `output`
re-reads a past job's output and re-verifies its
receipt locally, so the answer survives the terminal that first showed
it. `receipts --limit <n>` caps how many rows print, and `withdraw
--withdrawal-id <id>` reuses an id so a retried withdrawal moves money
once. `verify` reads the payout back from the buyer's own RPC
(`COVENANT_COMPUTE_RPC_URL`, or `--rpc-url`), never the coordinator's
suggestion. `whoami` needs no coordinator; every other command needs
`COVENANT_COMPUTE_COORDINATOR_URL`.

`lease open` rents a whole machine for a window instead of buying one
job: `--minutes <n>` (or `--duration-secs <n>`) sets the window,
`--rate <micro-usdc>` the price per second, and `--ssh-key <path>` the
OpenSSH public key you will use to reach the box. The window's ceiling
(rate times duration) is escrowed up front, billed by the second, and
the unused part refunded when you close; the escrow is still bounded by
`COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC`. `open` waits for the operator to
bring the machine up and prints its address (`--wait-secs <n>`, or
`--no-wait` to return as soon as the lease is placed). `lease view
<job-id>` shows where the machine is, how long it has run and what it has
cost so far; `lease close <job-id>` ends the session and settles the
meter to the seconds served. `--gpu-class`, `--min-vram-gb` and
`--min-reputation-bps` constrain the supply as they do for a buy.

`vault` keeps a secret with the coordinator without trusting it with the
contents. A value you seal (an SSH key, a model token, an `.env` file) is
encrypted on your machine before it is sent, so the coordinator stores
ciphertext it cannot read and hands back only to you. `vault put <label>`
seals a value passed inline, read from a file with `--file <path>`, or
piped in with `-`; `vault get <label>` fetches and opens it, writing the
raw bytes to stdout so a binary secret survives a pipe (`vault get
deploy-key > id_ed25519`). `vault ls` lists what you have stored and
marks which secrets this machine can open, and `vault rm <label>` deletes
one and drops its key.

The key that seals and opens each secret stays on this machine, in
`vault-keys.json` under `$COVENANT_COMPUTE_MCP_HOME`. It is never sent, so
the coordinator cannot read your secrets, and so losing that file means
losing them: the stored ciphertext cannot be opened without the key. Back
a key up with `vault key export <label>`, and restore it on another
machine with `vault key import <label> <key>`, carrying your identity file
across too so the coordinator recognizes you as the owner. Replacing a
different key already held for a label needs `--force`. The vault is
available only against a coordinator that serves it.

## As a library

`JobRequest` covers the same surface programmatically:
`dispatch_and_verify` signs the envelope, submits, polls the receipt
and verifies it before returning; `dispatch_streaming` does the same
while feeding the job's live output to a callback as it generates,
then reports whether the assembled feed matched the verified final
output (`poll_stream` is the underlying cursor read, and the
`submit_streaming`/`stream_and_verify` halves plus the `StreamJobs`
ledger are what a start/poll surface like the MCP pair builds on);
`list_verified_jobs`, `fetch_job_output`, `claim_deposit`,
`funds_with_deposit_info`, `withdraw`, `list_withdrawals` and
`dispute_job` mirror the read/money/dispute tools. `vault_store`,
`vault_fetch`, `vault_list` and `vault_delete` seal, store and open
client-sealed secrets, and `VaultKeyring` holds the sealing keys on disk
the way the CLI does. covenantd embeds
this crate for its native `compute.*` capabilities, so an agent's
compute purchases ride the same budget and audit machinery as
everything else it does. Hold the crate's `http_client()` rather than
a bare `reqwest::Client`: it stamps the wire protocol version on every
request, which is what lets a coordinator that raised its version
floor refuse a stale binary with "upgrade" instead of a parse error.

A withdrawal is a signed request, so a relay can't re-point the
recipient wallet, and its buyer-chosen id is the idempotency key end to
end: the books debit once, the transfer carries
`compute-withdrawal:v1:<buyer>:<id>` as its on-chain memo, and retrying
is always safe. The debit commits even if the transfer push fails
mid-flight; a push that never reached the chain is re-pushed by the
coordinator's sweep until it lands, while one whose outcome can't be
confirmed is held for reconciliation rather than sent again, so the
same withdrawal is never paid twice. `list_withdrawals` shows exactly
how far each one got.

The payout itself is verifiable by anyone holding the signed receipt,
with no trust in the coordinator: the receipt derives its own payout
memo, and `verify_payout_onchain` checks a fetched transaction
(`fetch_payout_transaction`, or any `getTransaction` in `jsonParsed`
form) succeeded, carries exactly that memo, and actually grew one
wallet's balance — returning how much, in what mint, to whom, all read
off the chain's own record. `verify_payout` composes the whole trail
into one call (history read, local receipt re-verification, chain
fetch, amount cross-check against the books) and is what
`compute.verify` serves. The RPC endpoint is always the buyer's own
configuration, never the coordinator's suggestion.

Money is integer micro-USDC end to end; jobs are refused, not
truncated, when a price ceiling or balance can't cover them.
