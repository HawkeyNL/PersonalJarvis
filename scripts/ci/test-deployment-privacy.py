import hashlib
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("privacy", Path(__file__).with_name("check-deployment-privacy.py"))
privacy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(privacy)


class PrivacyTests(unittest.TestCase):
    def test_reserved_examples_are_allowed(self):
        data = b"https://jarvis.example.com https://home.example.org https://jarvis.example.invalid\n192.0.2.10 198.51.100.20 203.0.113.30 10.23.45.10 10.23.45.0/24"
        self.assertEqual(list(privacy.matches(data)), [])

    def test_digest_detection_reports_line_without_exposing_value(self):
        fingerprints = {hashlib.sha256(b"203.0.113.30").hexdigest(): "test-public-ip"}
        self.assertEqual(list(privacy.matches(b"first\norigin=https://203.0.113.30:443", fingerprints)),
                         [(2, "test-public-ip")])

    def test_domain_suffix_and_case_are_detected(self):
        fingerprints = {hashlib.sha256(b"example.com").hexdigest(): "test-domain"}
        self.assertEqual(list(privacy.matches(b"https://Jarvis.Example.Com/path", fingerprints)),
                         [(1, "test-domain")])

    def test_binary_embedded_literal_is_detected(self):
        fingerprints = {hashlib.sha256(b"192.0.2.10").hexdigest(): "test-address"}
        self.assertEqual(list(privacy.matches(b"\x00\xff192.0.2.10\x00", fingerprints)),
                         [(1, "test-address")])


if __name__ == "__main__":
    unittest.main()
