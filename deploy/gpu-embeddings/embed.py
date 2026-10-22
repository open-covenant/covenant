"""Embed every line of a text file on the GPU with BAAI/bge-small-en-v1.5.

Usage: python3 embed.py <https URL of a UTF-8 text file>

Writes embeddings.npy, float32 with one L2-normalized 384-dimension row per
input line in input order, and report.json, which records what ran, on which
GPU, and the hashes needed to check the result independently. Lines are split
on "\\n" only: a trailing newline does not add a row and a "\\r" before it is
dropped. Rows match sentence-transformers' encode(normalize_embeddings=True)
for the same model revision.
"""

import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.request

MODEL = "BAAI/bge-small-en-v1.5"
REVISION = "5c38ec7c405ec4b44b94cc5a9bb96e735b38267a"
FILES = {
    "onnx/model.onnx": "828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35",
    "tokenizer.json": "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
}
MAX_LINES = 10_000
MAX_INPUT_BYTES = 4 * 1024 * 1024
MAX_TOKENS = 512
BATCH = 128


def fetch(url, limit):
    with urllib.request.urlopen(url, timeout=60) as response:
        body = response.read(limit + 1)
    if len(body) > limit:
        raise SystemExit(f"{url} is larger than {limit} bytes")
    return body


def model_file(name):
    """Downloads a pinned model file once and refuses it if its hash differs."""
    path = os.path.join("model", name)
    if not os.path.exists(path):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        url = f"https://huggingface.co/{MODEL}/resolve/{REVISION}/{name}"
        data = fetch(url, 512 * 1024 * 1024)
        with open(path, "wb") as f:
            f.write(data)
    with open(path, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    if digest != FILES[name]:
        raise SystemExit(f"{name} has SHA-256 {digest}, expected {FILES[name]}")
    return path


def split_lines(raw):
    lines = raw.decode("utf-8").split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return [line[:-1] if line.endswith("\r") else line for line in lines]


def embed(lines, providers):
    import numpy as np
    import onnxruntime as ort
    from tokenizers import Tokenizer

    tokenizer = Tokenizer.from_file(model_file("tokenizer.json"))
    tokenizer.enable_truncation(max_length=MAX_TOKENS)
    tokenizer.enable_padding(pad_id=0, pad_token="[PAD]")
    session = ort.InferenceSession(model_file("onnx/model.onnx"), providers=providers)
    if session.get_providers()[0] != providers[0]:
        raise SystemExit(f"{providers[0]} is unavailable; got {session.get_providers()}")
    names = {i.name for i in session.get_inputs()}
    rows = []
    for start in range(0, len(lines), BATCH):
        encoded = tokenizer.encode_batch(lines[start : start + BATCH])
        ids = np.array([e.ids for e in encoded], dtype=np.int64)
        feed = {
            "input_ids": ids,
            "attention_mask": np.array([e.attention_mask for e in encoded], dtype=np.int64),
        }
        if "token_type_ids" in names:
            feed["token_type_ids"] = np.zeros_like(ids)
        # bge pools the [CLS] token, then normalizes.
        cls = session.run(None, feed)[0][:, 0, :]
        rows.append(cls / np.linalg.norm(cls, axis=1, keepdims=True))
    return np.concatenate(rows).astype(np.float32), session


def nvidia_smi(*query):
    out = subprocess.run(
        ["nvidia-smi", *query, "--format=csv,noheader,nounits"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [line.split(", ") for line in out.strip().splitlines()]


def main():
    import numpy as np
    import onnxruntime as ort

    if len(sys.argv) != 2 or not sys.argv[1].startswith("https://"):
        raise SystemExit("usage: python3 embed.py <https URL of a UTF-8 text file>")
    url = sys.argv[1]
    timings = {}

    started = time.monotonic()
    raw = fetch(url, MAX_INPUT_BYTES)
    lines = split_lines(raw)
    if not 1 <= len(lines) <= MAX_LINES:
        raise SystemExit(f"expected 1 to {MAX_LINES} lines, got {len(lines)}")
    timings["download_input"] = time.monotonic() - started

    before = int(nvidia_smi("--query-gpu=memory.used")[0][0])
    started = time.monotonic()
    embeddings, session = embed(lines, ["CUDAExecutionProvider"])
    timings["load_and_embed"] = time.monotonic() - started
    # The session is still open, so the model is resident on the GPU. Containers
    # hide host PIDs from nvidia-smi, hence whole-device figures.
    during = int(nvidia_smi("--query-gpu=memory.used")[0][0])

    np.save("embeddings.npy", embeddings)
    with open("embeddings.npy", "rb") as f:
        embeddings_sha256 = hashlib.sha256(f.read()).hexdigest()
    name, driver, memory = nvidia_smi("--query-gpu=name,driver_version,memory.total")[0]

    report = {
        "model": MODEL,
        "revision": REVISION,
        "model_files_sha256": FILES,
        "pooling": "cls, L2-normalized",
        "input_url": url,
        "input_sha256": hashlib.sha256(raw).hexdigest(),
        "rows": int(embeddings.shape[0]),
        "dimensions": int(embeddings.shape[1]),
        "dtype": "float32",
        "embeddings_sha256": embeddings_sha256,
        "execution_provider": session.get_providers()[0],
        "onnxruntime": ort.__version__,
        "gpu": {"name": name, "driver": driver, "memory_mib": int(memory)},
        "gpu_memory_used_mib": {"before_model": before, "with_model_loaded": during},
        "seconds": {key: round(value, 2) for key, value in timings.items()},
    }
    with open("report.json", "w") as f:
        json.dump(report, f, indent=1)
    print(json.dumps(report))


if __name__ == "__main__":
    main()
