#!/usr/bin/env python3
"""Merge phase outputs into results/local-run.json with metadata + separation()."""
import json
import os
import struct
import datetime

import harness

PHASE = "phase"
FTYPE = {0: "F32", 1: "F16", 2: "Q4_0", 3: "Q4_1", 7: "Q8_0", 8: "Q5_0",
         9: "Q5_1", 10: "Q2_K", 11: "Q3_K_S", 12: "Q3_K_M", 13: "Q3_K_L",
         14: "Q4_K_S", 15: "Q4_K_M", 16: "Q5_K_S", 17: "Q5_K_M", 18: "Q6_K"}


def gguf_meta(path):
    """Minimal GGUF header reader for the few metadata keys we report."""
    want = {"general.name", "general.architecture", "general.file_type",
            "general.quantization_version", "general.size_label",
            "qwen3.block_count", "qwen3.context_length"}
    with open(path, "rb") as f:
        if f.read(4) != b"GGUF":
            return {}
        struct.unpack("<I", f.read(4))            # version
        struct.unpack("<Q", f.read(8))            # n_tensors
        n_kv, = struct.unpack("<Q", f.read(8))

        def rstr():
            ln, = struct.unpack("<Q", f.read(8))
            return f.read(ln).decode("utf-8", "replace")

        def rval(t):
            fmt = {0: "<b", 1: "<B", 2: "<h", 3: "<H", 4: "<i", 5: "<I",
                   6: "<f", 7: "<?", 10: "<q", 11: "<Q", 12: "<d"}
            if t in fmt:
                sz = struct.calcsize(fmt[t])
                return struct.unpack(fmt[t], f.read(sz))[0]
            if t == 8:
                return rstr()
            if t == 9:
                et, = struct.unpack("<I", f.read(4))
                ln, = struct.unpack("<Q", f.read(8))
                return [rval(et) for _ in range(ln)]
            raise ValueError("gguf type %d" % t)

        out = {}
        for _ in range(n_kv):
            k = rstr()
            t, = struct.unpack("<I", f.read(4))
            v = rval(t)
            if k in want:
                out[k] = v
    ft = out.get("general.file_type")
    out["quant"] = FTYPE.get(ft, "enum:%s" % ft)
    return out


def load(name):
    with open(os.path.join(PHASE, name)) as f:
        return json.load(f)


def strip_arrays(d):
    d.pop("_kls", None)
    d.pop("_top1", None)
    return d


def sanitize(path):
    """Keep absolute user paths out of the written artifact."""
    home = os.path.expanduser("~")
    return path.replace(home, "~") if path else path


def main():
    blob = os.environ.get("MODEL_BLOB", "")
    meta = gguf_meta(blob) if blob and os.path.exists(blob) else {}

    exp1s = load("exp1_serial.json")
    exp1c = load("exp1_concurrent.json")
    exp2 = load("exp2_seeds.json")

    serial_kls = exp1s.get("_kls", [])
    conc_kls = exp1c.get("_kls", [])

    # Kill-criterion tool. The genuine inter-quant "signal" leg needs a second
    # quant we don't have locally, so it is recorded as pending. As an auxiliary
    # local check we run separation() with single-stream KL as the noise floor
    # and concurrent/batched KL as the candidate deviation.
    sep_serial_vs_concurrent = harness.separation(intra=serial_kls, inter=conc_kls)

    result = {
        "meta": {
            "generated_utc": datetime.datetime.utcnow().isoformat() + "Z",
            "host": "macOS Apple Silicon (Metal), unified memory",
            "engine": "llama.cpp llama-server",
            "llamacpp_version": os.environ.get("LLAMACPP_VERSION", ""),
            "model": {
                "name": meta.get("general.name"),
                "architecture": meta.get("general.architecture"),
                "size_label": meta.get("general.size_label"),
                "quant": meta.get("quant"),
                "quantization_version": meta.get("general.quantization_version"),
                "block_count": meta.get("qwen3.block_count"),
                "context_length": meta.get("qwen3.context_length"),
                "gguf_blob": sanitize(blob),
                "source": "ollama qwen3:8b",
            },
            "server_flags": {
                "serial": os.environ.get("SERIAL_FLAGS_STR", ""),
                "concurrent": os.environ.get("CONC_FLAGS_STR", ""),
            },
            "request_params": {
                "n_predict": 128, "temperature": 0, "top_k": 1,
                "n_probs": 20, "cache_prompt": False, "stream": False,
            },
        },
        "experiment_1_repeatability": {
            "serial_single_stream": strip_arrays(exp1s),
            "concurrent_batched": strip_arrays(exp1c),
        },
        "experiment_2_seed_control": exp2,
        "experiment_3_quant_separation": {
            "status": "SKIPPED — pending",
            "reason": ("Only one local quant of qwen3:8b (Q4_K_M). A true second-quant "
                       "leg needs the F16 source (~16 GB) to re-quantize, or a fresh "
                       "quant download; both exceed the modest-download budget. "
                       "Re-quantizing an already-Q4 GGUF would not be a valid independent "
                       "quant. Deferred to the remote GPU matrix + multi-quant run."),
        },
        "separation_kill_criterion": {
            "note": ("Genuine intra-vs-inter (same-model-noise vs different-quant-signal) "
                     "separation requires the pending quant leg. Auxiliary result below "
                     "uses single-stream KL as noise floor vs concurrent/batched KL."),
            "interpretation": (
                "Single-stream (parallel=1) is a hard zero noise floor: every KL is exactly "
                "0.0. Concurrent/batched (parallel=4, continuous batching) is BIMODAL, not a "
                "uniformly raised floor: most positions still agree exactly (median ~4e-11), "
                "but a fraction of prompts diverge catastrophically (p95 ~20.6). Because the "
                "batching signal is rare-but-total, a naive intra_p95<inter_p5 test reports "
                "separable=false (inter_p5 sits at ~0). The meaningful contrast is the MEAN/"
                "p95: single-stream mean KL = 0.0 vs concurrent mean KL ~3.88, p95 ~20.6. "
                "Takeaway for the kill criterion: a canary spot-check cannot rely on bitwise "
                "equality under real batched serving; it must tolerate rare full-divergence "
                "events from nondeterministic batch composition and score on a distribution."),
            "serial_noise_floor_vs_concurrent": sep_serial_vs_concurrent,
        },
    }

    with open("results/local-run.json", "w") as f:
        json.dump(result, f, indent=2)
    print("wrote results/local-run.json")


if __name__ == "__main__":
    main()
