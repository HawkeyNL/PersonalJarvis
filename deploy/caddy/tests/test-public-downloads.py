"""Exercise the reviewed routing with an isolated, rootless Caddy process."""
import http.server
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[3]


class ProtectedAPI(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(401)
        self.send_header("X-Fixture-API", "protected")
        self.end_headers()
        self.wfile.write(b"authentication required")

    def log_message(self, *_args):
        pass


@unittest.skipUnless(shutil.which("caddy"), "Caddy required for actual routing test")
class PublicDownloadTests(unittest.TestCase):
    def test_exact_public_page_leaves_update_api_protected(self):
        with tempfile.TemporaryDirectory(prefix="jarvis-caddy-test-") as temporary:
            directory = Path(temporary)
            page = ROOT / "deploy/caddy/downloads/index.html"
            archive = directory / "public"
            ipa_dir = archive / "ios/v0.1.0"
            ipa_dir.mkdir(parents=True)
            (archive / "index.html").write_bytes(page.read_bytes())
            (ipa_dir / "Jarvis_0.1.0_ios_arm64_unsigned.ipa").write_bytes(b"fixture-ipa-not-installable")
            (ipa_dir / "approved.json").write_text('{"fixture":"not public"}')
            client_paths = []
            for target, suffix in [('linux-x86_64', '.AppImage'), ('windows-x86_64', '.exe'),
                                   ('macos-arm64', '.dmg'), ('android-universal', '.apk'),
                                   ('ios-arm64', '_unsigned.ipa')]:
                name = f'Jarvis_0.1.0_{target.replace("-", "_")}{suffix}'
                relative = f'releases/v0.1.0/{target}/{name}'
                path = archive / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'fixture-installer-not-installable')
                client_paths.append('/downloads/' + relative)
            (archive / 'releases/v0.1.0/manifest.json').write_text('{"fixture":"not public"}')
            api = http.server.ThreadingHTTPServer(("127.0.0.1", 0), ProtectedAPI)
            worker = threading.Thread(target=api.serve_forever, daemon=True)
            worker.start()
            process = None
            env = {"PATH": os.defpath, "JARVIS_PUBLIC_HOSTNAME": "jarvis.example.invalid",
                   "XDG_DATA_HOME": str(directory / "data"), "XDG_CONFIG_HOME": str(directory / "config")}
            caddy = shutil.which("caddy")
            try:
                result = subprocess.run([caddy, "adapt", "--config", str(ROOT / "deploy/caddy/Caddyfile"),
                                         "--adapter", "caddyfile"], env=env, capture_output=True, check=True, timeout=10)
                # Test the actual adapted route graph, changing only host/listen,
                # fixture storage and the upstream. No production state accessed.
                raw = result.stdout.decode().replace("/var/lib/jarvis-public-downloads", str(archive))
                raw = raw.replace("127.0.0.1:8080", f"127.0.0.1:{api.server_port}")
                config = json.loads(raw)
                config["admin"] = {"disabled": True, "config": {"persist": False}}
                config["apps"].pop("tls", None)
                server = next(iter(config["apps"]["http"]["servers"].values()))
                with socket.socket() as reservation:
                    reservation.bind(("127.0.0.1", 0))
                    port = reservation.getsockname()[1]
                server["listen"] = [f"127.0.0.1:{port}"]
                server.pop("tls_connection_policies", None)
                server["automatic_https"] = {"disable": True}
                config_path = directory / "caddy.json"
                config_path.write_text(json.dumps(config))
                with (directory / "caddy.log").open("wb") as log:
                    process = subprocess.Popen([caddy, "run", "--config", str(config_path)],
                                               env=env, cwd=directory, stdout=log, stderr=log)
                    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

                    def get(path):
                        request = urllib.request.Request(f"http://127.0.0.1:{port}{path}",
                                                         headers={"Host": "jarvis.example.invalid"})
                        try:
                            return opener.open(request, timeout=2)
                        except urllib.error.HTTPError as error:
                            return error

                    for _ in range(100):
                        if process.poll() is not None:
                            self.fail((directory / "caddy.log").read_text())
                        try:
                            with get("/downloads") as response:
                                self.assertEqual(response.status, 200)
                            break
                        except urllib.error.URLError:
                            time.sleep(0.05)
                    else:
                        self.fail("Isolated Caddy did not become ready")
                    for path in ("/downloads", "/downloads/"):
                        with get(path) as response:
                            self.assertEqual(response.status, 200)
                            self.assertEqual(response.read(), page.read_bytes())
                            self.assertIn("default-src 'none'", response.headers["Content-Security-Policy"])
                    with get('/downloads/ios/v0.1.0/Jarvis_0.1.0_ios_arm64_unsigned.ipa') as response:
                        self.assertEqual(response.status, 200)
                        self.assertEqual(response.read(), b'fixture-ipa-not-installable')
                        self.assertEqual(response.headers['Content-Disposition'], 'attachment')
                    with get('/downloads/ios/v0.2.0/Jarvis_0.2.0_ios_arm64_unsigned.ipa') as response:
                        self.assertEqual(response.status, 404)
                    for path in client_paths:
                        with get(path) as response:
                            self.assertEqual(response.status, 200, path)
                            self.assertEqual(response.read(), b'fixture-installer-not-installable')
                            self.assertEqual(response.headers['Content-Disposition'], 'attachment')
                    for path in ("/v1/app-updates/capability", "/v1/app-updates/latest.json",
                                 "/v1/events", "/downloads/manifest.json", "/downloads/index.html",
                                 "/downloads/.env", "/downloads/%2e%2e/secret", "/index.html",
                                 '/downloads/ios/v0.1.0/approved.json', '/downloads/ios/v0.1.0/',
                                 '/downloads/releases/v0.1.0/manifest.json', '/downloads/releases/v0.1.0/',
                                 '/downloads/releases/v0.1.0/macos-arm64/Jarvis_0.1.0_macos_arm64.app.tar.gz',
                                 '/downloads/releases/v0.1.0/android-universal/Jarvis_0.1.0_android_universal.aab',
                                 '/downloads/ios/.staging-x/Jarvis_0.1.0_ios_arm64_unsigned.ipa',
                                 '/downloads/ios/v0.1.0/%2e%2e/approved.json'):
                        with get(path) as response:
                            self.assertEqual(response.status, 401, path)
                            self.assertEqual(response.headers.get("X-Fixture-API"), "protected", path)
                            self.assertEqual(response.read(), b"authentication required")
            finally:
                if process is not None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
                api.shutdown()
                api.server_close()
                worker.join(timeout=5)


if __name__ == "__main__":
    unittest.main()
