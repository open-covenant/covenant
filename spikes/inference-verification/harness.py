#!/usr/bin/env python3
"""
Inference-determinism spike harness.

Stdlib + numpy + urllib only (no pip installs). Pure client: it talks to an
already-running inference server on a given port. Server lifecycle (flags,
restart between parallel/serial modes) is orchestrated by run_local.sh so the
harness stays engine-agnostic and a remote vLLM leg can be dropped in later by
implementing the same Engine.run() contract.

Engine contract:
    run(prompt: str, seed: int, n_predict: int) -> {
        "token_ids": list[int],
        "tokens":    list[str],
        "logprobs":  list[list[[token_str, logprob_float]]]   # per position, top-K
    }
"""

import json
import math
import sys
import time
import urllib.request
import urllib.error
from concurrent.futures import ThreadPoolExecutor

import numpy as np

EPS = 1e-9
LOG_FLOOR = -25.0   # logprob assigned to a token absent from the other run's top-K


# --------------------------------------------------------------------------
# Engine adapters
# --------------------------------------------------------------------------
class LlamaServerEngine:
    """Adapter for a llama.cpp llama-server /completion endpoint."""

    def __init__(self, port, host="127.0.0.1", n_probs=20, timeout=300):
        self.base = "http://%s:%d" % (host, port)
        self.n_probs = n_probs
        self.timeout = timeout

    def run(self, prompt, seed, n_predict=128):
        body = {
            "prompt": prompt,
            "n_predict": n_predict,
            "temperature": 0,
            "top_k": 1,
            "seed": seed,
            "n_probs": self.n_probs,
            "cache_prompt": False,
            "stream": False,
        }
        data = json.dumps(body).encode("utf-8")
        req = urllib.request.Request(
            self.base + "/completion", data=data,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=self.timeout) as resp:
            payload = json.loads(resp.read().decode("utf-8"))
        return parse_completion(payload)

    def props(self):
        with urllib.request.urlopen(self.base + "/props", timeout=30) as resp:
            return json.loads(resp.read().decode("utf-8"))


def parse_completion(payload):
    """Normalise a llama-server /completion JSON response into the engine contract.

    Handles the modern shape: top-level `tokens` (ids) + `completion_probabilities`,
    each entry carrying `top_logprobs: [{id, token, logprob|prob}]`. If only `prob`
    is present (post_sampling_probs=true) it is converted to a logprob.
    """
    cprobs = payload.get("completion_probabilities")
    if cprobs is None:
        # Fallback shapes seen across versions.
        cprobs = payload.get("logprobs") or payload.get("probs") or []

    token_ids, tokens, logprobs = [], [], []
    for pos in cprobs:
        token_ids.append(pos.get("id"))
        tokens.append(pos.get("token", ""))
        tops = pos.get("top_logprobs") or pos.get("probs") or pos.get("top_probs") or []
        row = []
        for cand in tops:
            tok = cand.get("token", "")
            if "logprob" in cand and cand["logprob"] is not None:
                lp = float(cand["logprob"])
            elif "prob" in cand and cand["prob"] is not None:
                lp = math.log(max(float(cand["prob"]), EPS))
            else:
                continue
            row.append([tok, lp])
        logprobs.append(row)

    # If completion_probabilities was empty, fall back to top-level tokens list.
    if not token_ids and payload.get("tokens"):
        token_ids = list(payload["tokens"])

    return {
        "token_ids": token_ids,
        "tokens": tokens,
        "logprobs": logprobs,
        "content": payload.get("content", ""),
    }


# --------------------------------------------------------------------------
# Metrics
# --------------------------------------------------------------------------
def token_agreement(a, b):
    """Full-sequence exact match + longest-common-prefix ratio over token ids."""
    ta, tb = a["token_ids"], b["token_ids"]
    exact = (ta == tb)
    lcp = 0
    for x, y in zip(ta, tb):
        if x == y:
            lcp += 1
        else:
            break
    denom = max(len(ta), len(tb), 1)
    return {
        "exact_match": bool(exact),
        "lcp": lcp,
        "max_len": denom,
        "lcp_ratio": lcp / denom,
        "len_a": len(ta),
        "len_b": len(tb),
    }


def _sym_kl_at_position(row_a, row_b):
    """Symmetric KL over the union of two top-K logprob distributions.

    Align by token string, take the union, fill missing tokens with a log floor,
    renormalise each side over the union, then 0.5*(KL(P||Q)+KL(Q||P)).
    """
    da = {t: lp for t, lp in row_a}
    db = {t: lp for t, lp in row_b}
    keys = list(set(da) | set(db))
    if not keys:
        return None
    la = np.array([da.get(k, LOG_FLOOR) for k in keys], dtype=np.float64)
    lb = np.array([db.get(k, LOG_FLOOR) for k in keys], dtype=np.float64)
    # log-sum-exp normalise to proper distributions over the union
    pa = np.exp(la - _logsumexp(la))
    pb = np.exp(lb - _logsumexp(lb))
    pa = np.clip(pa, EPS, 1.0)
    pb = np.clip(pb, EPS, 1.0)
    pa /= pa.sum()
    pb /= pb.sum()
    kl_ab = float(np.sum(pa * np.log(pa / pb)))
    kl_ba = float(np.sum(pb * np.log(pb / pa)))
    return 0.5 * (kl_ab + kl_ba)


def _logsumexp(x):
    m = np.max(x)
    return m + np.log(np.sum(np.exp(x - m)))


def logit_divergence(a, b):
    """Per-position symmetric KL + top-1 logprob abs delta between two runs.

    Returns per-position arrays and aggregate stats. Positions compared up to the
    shorter of the two sequences.
    """
    la, lb = a["logprobs"], b["logprobs"]
    n = min(len(la), len(lb))
    kls, top1_deltas = [], []
    for i in range(n):
        if not la[i] or not lb[i]:
            continue
        kl = _sym_kl_at_position(la[i], lb[i])
        if kl is not None:
            kls.append(kl)
        # top-1 logprob abs delta: each run's own argmax (first entry) logprob
        top1_a = la[i][0][1]
        top1_b = lb[i][0][1]
        top1_deltas.append(abs(top1_a - top1_b))
    return {
        "n_positions": n,
        "kl": kls,
        "top1_delta": top1_deltas,
        "kl_mean": _f(np.mean(kls)) if kls else None,
        "kl_median": _f(np.median(kls)) if kls else None,
        "kl_p95": _f(np.percentile(kls, 95)) if kls else None,
        "kl_max": _f(np.max(kls)) if kls else None,
        "top1_delta_mean": _f(np.mean(top1_deltas)) if top1_deltas else None,
        "top1_delta_p95": _f(np.percentile(top1_deltas, 95)) if top1_deltas else None,
        "top1_delta_max": _f(np.max(top1_deltas)) if top1_deltas else None,
    }


def separation(intra, inter):
    """Kill-criterion tool: do intra-run (noise) and inter-condition (signal) KL
    distributions overlap? Separable if intra-p95 < inter-p5."""
    intra = [x for x in intra if x is not None]
    inter = [x for x in inter if x is not None]
    out = {
        "intra": _dist_stats(intra),
        "inter": _dist_stats(inter),
    }
    if intra and inter:
        intra_p95 = float(np.percentile(intra, 95))
        inter_p5 = float(np.percentile(inter, 5))
        out["intra_p95"] = intra_p95
        out["inter_p5"] = inter_p5
        out["separable"] = bool(intra_p95 < inter_p5)
        out["gap"] = inter_p5 - intra_p95
    else:
        out["separable"] = None
        out["note"] = "one distribution empty (inter-condition experiment not run)"
    return out


def _dist_stats(xs):
    if not xs:
        return {"n": 0}
    a = np.array(xs, dtype=np.float64)
    return {
        "n": len(xs),
        "min": _f(np.min(a)),
        "median": _f(np.median(a)),
        "p95": _f(np.percentile(a, 95)),
        "max": _f(np.max(a)),
        "mean": _f(np.mean(a)),
    }


def _f(x):
    return float(x)


# --------------------------------------------------------------------------
# Experiments
# --------------------------------------------------------------------------
def load_prompts(path):
    with open(path) as f:
        return json.load(f)


def exp_repeatability(engine, prompts, R=5, seed=42, n_predict=128, concurrent=False):
    """R identical requests per prompt, same seed. Compare run 1 vs runs 2..R.

    concurrent=True fires all R requests simultaneously (exercises continuous
    batching / the realistic serving path); False runs them strictly serially.
    """
    per_prompt = []
    all_kls, all_top1 = [], []
    all_identical = True

    for p in prompts:
        if concurrent:
            with ThreadPoolExecutor(max_workers=R) as ex:
                futs = [ex.submit(engine.run, p["prompt"], seed, n_predict) for _ in range(R)]
                runs = [f.result() for f in futs]
        else:
            runs = [engine.run(p["prompt"], seed, n_predict) for _ in range(R)]

        ref = runs[0]
        contents_identical = all(r["content"] == ref["content"] for r in runs[1:])
        cmps = []
        for r in runs[1:]:
            agree = token_agreement(ref, r)
            div = logit_divergence(ref, r)
            all_kls.extend(div["kl"])
            all_top1.extend(div["top1_delta"])
            cmps.append({"agreement": agree, "divergence": _slim(div)})
            if not agree["exact_match"]:
                all_identical = False
        if not contents_identical:
            all_identical = False
        per_prompt.append({
            "id": p["id"],
            "kind": p["kind"],
            "ref_len": len(ref["token_ids"]),
            "contents_identical": contents_identical,
            "comparisons": cmps,
        })

    return {
        "R": R,
        "seed": seed,
        "n_predict": n_predict,
        "concurrent": concurrent,
        "all_runs_token_identical": all_identical,
        "aggregate_kl": _dist_stats(all_kls),
        "aggregate_top1_delta": _dist_stats(all_top1),
        "_kls": all_kls,          # kept for cross-experiment separation()
        "_top1": all_top1,
        "per_prompt": per_prompt,
    }


def exp_seed_control(engine, prompts, seeds=(1, 42, 999), n_predict=128):
    """Same prompt, different seeds at temp0. Greedy should ignore the seed."""
    per_prompt = []
    all_identical = True
    for p in prompts:
        runs = {s: engine.run(p["prompt"], s, n_predict) for s in seeds}
        ref = runs[seeds[0]]
        cmps = []
        for s in seeds[1:]:
            agree = token_agreement(ref, runs[s])
            div = logit_divergence(ref, runs[s])
            cmps.append({"seed": s, "agreement": agree, "divergence": _slim(div)})
            if not agree["exact_match"]:
                all_identical = False
        per_prompt.append({"id": p["id"], "kind": p["kind"],
                           "ref_seed": seeds[0], "comparisons": cmps})
    return {
        "seeds": list(seeds),
        "n_predict": n_predict,
        "greedy_ignores_seed": all_identical,
        "per_prompt": per_prompt,
    }


def exp_quant_separation(engine_a, engine_b, prompts, seed=42, n_predict=128):
    """Inter-quantization divergence: same prompts on two quants of the same model."""
    per_prompt = []
    all_kls, all_top1 = [], []
    any_token_diff = False
    for p in prompts:
        ra = engine_a.run(p["prompt"], seed, n_predict)
        rb = engine_b.run(p["prompt"], seed, n_predict)
        agree = token_agreement(ra, rb)
        div = logit_divergence(ra, rb)
        all_kls.extend(div["kl"])
        all_top1.extend(div["top1_delta"])
        if not agree["exact_match"]:
            any_token_diff = True
        per_prompt.append({"id": p["id"], "kind": p["kind"],
                           "agreement": agree, "divergence": _slim(div)})
    return {
        "seed": seed,
        "n_predict": n_predict,
        "any_token_divergence": any_token_diff,
        "aggregate_kl": _dist_stats(all_kls),
        "aggregate_top1_delta": _dist_stats(all_top1),
        "_kls": all_kls,
        "_top1": all_top1,
        "per_prompt": per_prompt,
    }


def _slim(div):
    """Drop the big per-position arrays from a divergence dict for JSON output."""
    return {k: v for k, v in div.items() if k not in ("kl", "top1_delta")}


# --------------------------------------------------------------------------
# CLI: run one experiment phase, print JSON to stdout
# --------------------------------------------------------------------------
def main():
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["exp1_serial", "exp1_concurrent", "exp2_seeds", "probe"])
    ap.add_argument("--port", type=int, default=8099)
    ap.add_argument("--prompts", default="prompts.json")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--n-predict", type=int, default=128)
    args = ap.parse_args()

    engine = LlamaServerEngine(port=args.port)
    prompts = load_prompts(args.prompts)

    if args.mode == "probe":
        r = engine.run(prompts[0]["prompt"], args.seed, 4)
        print(json.dumps({"content": r["content"],
                          "token_ids": r["token_ids"],
                          "logprobs_pos0": r["logprobs"][0][:3]}, indent=2))
        return
    if args.mode == "exp1_serial":
        out = exp_repeatability(engine, prompts, R=args.runs, seed=args.seed,
                                n_predict=args.n_predict, concurrent=False)
    elif args.mode == "exp1_concurrent":
        out = exp_repeatability(engine, prompts, R=args.runs, seed=args.seed,
                                n_predict=args.n_predict, concurrent=True)
    elif args.mode == "exp2_seeds":
        out = exp_seed_control(engine, prompts, seeds=(1, 42, 999),
                               n_predict=args.n_predict)
    print(json.dumps(out))


if __name__ == "__main__":
    main()
