import json
from http.server import BaseHTTPRequestHandler
from pathlib import Path
from socketserver import UnixStreamServer
import tempfile
import threading
import unittest

import benchmark


class FixtureHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        assert self.path == "/v1/systemone"
        assert "Authorization" not in self.headers
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert "model" not in body
        assert len(body["questions"]) == 1
        response = json.dumps({"answers": {"work_kind": {
            "choice": "coding", "answer_confidence": 0.97}}}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)

    def log_message(self, *_args):
        pass


class BenchmarkTests(unittest.TestCase):
    def test_fixture_covers_every_core_label(self):
        corpus = json.loads(Path(__file__).with_name("intent-corpus.json").read_text())
        self.assertEqual(set(case["expected"] for case in corpus), set(benchmark.LABELS))
        self.assertGreaterEqual(len(corpus), 40)
        self.assertLessEqual(len(corpus), 60)
        for label in benchmark.LABELS:
            self.assertGreaterEqual(sum(case["expected"] == label for case in corpus), 8)

    def test_errors_remain_in_class_denominator_and_accepted_accuracy(self):
        cases = [{"text": text, "expected": "coding"} for text in ("error", "wrong", "correct")]
        def predictor(text, _timeout):
            if text == "error":
                raise OSError("fixture unavailable")
            return ("coding" if text == "correct" else "research", 0.97, 1.0)
        report = benchmark.evaluate_cases(cases, 0.95, 1.0, predictor)
        self.assertEqual(report["per_class"]["coding"], {"correct": 1, "total": 3})
        self.assertEqual(report["errors"], 1)
        self.assertEqual(report["confusion"]["coding"]["unavailable"], 1)
        self.assertEqual(report["accepted_count"], 2)
        self.assertEqual(report["accepted_correct"], 1)
        self.assertEqual(report["accepted_accuracy"], 0.5)
        self.assertEqual(report["coverage_at_threshold"], 0.6667)
        self.assertEqual(report["jev_comparison"], {"available": False})

    def test_zero_accepted_has_null_accuracy(self):
        report = benchmark.evaluate_cases(
            [{"text": "hello", "expected": "conversation"}], 0.95, 1.0,
            lambda _text, _timeout: ("conversation", 0.5, 1.0))
        self.assertEqual(report["accepted_count"], 0)
        self.assertIsNone(report["accepted_accuracy"])
        self.assertEqual(report["coverage_at_threshold"], 0)

    def test_bounded_local_request_and_metrics(self):
        with tempfile.TemporaryDirectory() as directory:
            path = f"{directory}/laya.sock"
            server = UnixStreamServer(path, FixtureHandler)
            worker = threading.Thread(target=server.serve_forever, daemon=True)
            worker.start()
            original = benchmark.SOCKET_PATH
            benchmark.SOCKET_PATH = path
            try:
                kind, confidence, latency = benchmark.predict("Review this code", 1.0)
                self.assertEqual(kind, "coding")
                self.assertEqual(confidence, 0.97)
                self.assertGreaterEqual(latency, 0)
                self.assertEqual(benchmark.percentile([1, 2, 3, 4], 0.95), 4)
            finally:
                benchmark.SOCKET_PATH = original
                server.shutdown()
                server.server_close()
                worker.join(timeout=2)


if __name__ == "__main__":
    unittest.main()
