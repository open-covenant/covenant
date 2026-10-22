"""Check a GPU embedding result on your own CPU.

Usage: python3 verify.py embeddings.npy report.json <input file path or https URL> [rows]

Needs: pip install sentence-transformers numpy
Recomputes `rows` random lines (default 20) with the same model revision and
passes when every row matches with a cosine similarity of at least 0.999. It
also checks that the files are the ones report.json describes.
"""

import hashlib
import json
import random
import sys
import urllib.request

import numpy as np
from sentence_transformers import SentenceTransformer

THRESHOLD = 0.999


def read(source):
    if source.startswith("https://"):
        with urllib.request.urlopen(source, timeout=60) as response:
            return response.read()
    with open(source, "rb") as f:
        return f.read()


def split_lines(raw):
    # The same rule embed.py uses.
    lines = raw.decode("utf-8").split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return [line[:-1] if line.endswith("\r") else line for line in lines]


def main():
    if len(sys.argv) not in (4, 5):
        raise SystemExit(__doc__)
    embeddings_path, report_path, source = sys.argv[1:4]
    count = int(sys.argv[4]) if len(sys.argv) == 5 else 20
    report = json.load(open(report_path))
    raw = read(source)
    lines = split_lines(raw)

    with open(embeddings_path, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    checks = {
        "embeddings file matches report": digest == report["embeddings_sha256"],
        "input matches report": hashlib.sha256(raw).hexdigest() == report["input_sha256"],
    }
    embeddings = np.load(embeddings_path)
    checks["one row per line"] = embeddings.shape == (len(lines), report["dimensions"])

    model = SentenceTransformer(report["model"], revision=report["revision"], device="cpu")
    picked = sorted(random.sample(range(len(lines)), min(count, len(lines))))
    reference = model.encode([lines[i] for i in picked], normalize_embeddings=True)
    cosines = (embeddings[picked] * reference).sum(axis=1)
    checks[f"{len(picked)} rows at cosine >= {THRESHOLD}"] = bool(cosines.min() >= THRESHOLD)

    for name, ok in checks.items():
        print(f"{'PASS' if ok else 'FAIL'}  {name}")
    print(f"lowest cosine similarity: {cosines.min():.7f} (row {picked[int(cosines.argmin())]})")
    sys.exit(0 if all(checks.values()) else 1)


if __name__ == "__main__":
    main()
