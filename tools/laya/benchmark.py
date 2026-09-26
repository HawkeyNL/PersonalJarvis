#!/usr/bin/env python3
"""Owner-operated local Laya benchmark. No secrets, model downloads or mutations."""

import argparse
from collections import Counter, defaultdict
from http.client import HTTPConnection
import json
import math
import os
from pathlib import Path
import socket
import statistics
import time

SOCKET_PATH = "/run/jarvis-laya.sock"
LABELS = ("conversation", "quick_answer", "research", "coding", "action_request")
CRITERIA = {
    "conversation": "General explanation, creative discussion or ordinary chat.",
    "quick_answer": "Short factual or simple utility question that needs no tools or deep reasoning.",
    "research": "Needs current sources, evidence gathering or comparison.",
    "coding": "Software development, debugging, code review or architecture.",
    "action_request": "Requests a side effect such as saving a note, reminder, system change or transaction.",
}


class LocalConnection(HTTPConnection):
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(SOCKET_PATH)


def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    return round(ordered[math.ceil(fraction * len(ordered)) - 1], 1)


def predict(text, timeout):
    body = json.dumps({"state": {"user_request": text},
        "questions": {"work_kind": {"type": "choice", "instructions":
            "Classify the user's primary request. This is advisory routing only; do not authorize or execute actions.",
            "criteria": CRITERIA}}}).encode()
    started = time.monotonic()
    connection = LocalConnection("jarvis-laya.local", timeout=timeout)
    try:
        connection.request("POST", "/v1/systemone", body, {"Content-Type": "application/json"})
        response = connection.getresponse()
        if response.status != 200:
            raise ValueError("local classifier did not return HTTP 200")
        raw = response.read(16385)
    finally:
        connection.close()
    latency_ms = (time.monotonic() - started) * 1000
    if len(raw) > 16384:
        raise ValueError("local classifier response exceeded 16 KiB")
    answer = json.loads(raw)["answers"]["work_kind"]
    choice = answer["choice"]
    confidence = answer["answer_confidence"]
    if choice not in LABELS or not isinstance(confidence, (int, float)) or not math.isfinite(confidence) or not 0 <= confidence <= 1:
        raise ValueError("invalid classifier answer")
    return choice, confidence, latency_ms


def process_rss_kb(pid):
    if pid is None:
        return None
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1])
    return None


def process_cpu_seconds(pid):
    if pid is None:
        return None
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def evaluate_cases(cases, threshold, timeout, predictor=predict):
    """Every labeled fixture counts; unavailable predictions are incorrect.

    Coverage is accepted/total, and accepted accuracy is correct/accepted.
    This local-only evaluator never calls Jev or a generative provider.
    """
    counts = Counter()
    per_class = defaultdict(Counter)
    confusion = defaultdict(Counter)
    latencies = []
    errors = 0
    for case in cases:
        expected, text = case["expected"], case["text"]
        if expected not in LABELS or not isinstance(text, str) or not 1 <= len(text) <= 4096:
            raise ValueError("invalid fixture")
        per_class[expected]["total"] += 1
        try:
            predicted, confidence, ms = predictor(text, timeout)
            latencies.append(ms)
            accepted = confidence >= threshold
            correct = predicted == expected
            counts["accepted"] += accepted
            counts["accepted_correct"] += accepted and correct
            counts["correct"] += correct
            per_class[expected]["correct"] += correct
            confusion[expected][predicted] += 1
        except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError):
            errors += 1
            confusion[expected]["unavailable"] += 1
    total = len(cases)
    accepted = counts["accepted"]
    return {
        "count": total, "errors": errors,
        "accuracy": round(counts["correct"] / total, 4),
        "accepted_count": accepted,
        "accepted_correct": counts["accepted_correct"],
        "accepted_accuracy": round(counts["accepted_correct"] / accepted, 4) if accepted else None,
        "coverage_at_threshold": round(accepted / total, 4),
        "fallback_percentage": round(100 * (1 - accepted / total), 2),
        "jev_comparison": {"available": False},
        "per_class": {label: {"correct": per_class[label]["correct"],
            "total": per_class[label]["total"]} for label in LABELS},
        "confusion": dict(confusion),
        "p50_ms": percentile(latencies, 0.5), "p95_ms": percentile(latencies, 0.95),
        "p99_ms": percentile(latencies, 0.99) if len(latencies) >= 100 else None,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, default=Path(__file__).with_name("intent-corpus.json"))
    parser.add_argument("--threshold", type=float, default=0.95)
    parser.add_argument("--timeout", type=float, default=5.0)
    parser.add_argument("--pid", type=int, help="optional jarvis-laya MainPID for RSS")
    parser.add_argument("--cold-load-ms", type=float, help="independently measured startup/preload time")
    args = parser.parse_args()
    if not 0 <= args.threshold <= 1 or not 0.1 <= args.timeout <= 10:
        parser.error("threshold or timeout outside safe range")
    cases = json.loads(args.corpus.read_text())
    if not isinstance(cases, list) or not 1 <= len(cases) <= 10000:
        parser.error("corpus must contain 1-10000 entries")
    cpu_before = process_cpu_seconds(args.pid)
    started = time.monotonic()
    try:
        metrics = evaluate_cases(cases, args.threshold, args.timeout)
    except ValueError as error:
        parser.error(str(error))
    elapsed = time.monotonic() - started
    cpu_after = process_cpu_seconds(args.pid)
    result = {
        **metrics,
        "requests_per_second": round(len(cases) / elapsed, 3),
        "rss_kb": process_rss_kb(args.pid),
        "cold_load_ms": args.cold_load_ms,
        "cpu_percent_of_one_core": round(100 * (cpu_after - cpu_before) / elapsed, 2)
            if cpu_before is not None and cpu_after is not None else None,
    }
    print(json.dumps(result, indent=2, sort_keys=True))
    if metrics["errors"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
