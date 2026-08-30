import assert from "node:assert/strict";
import test from "node:test";

import { capabilitiesFor } from "../src/platform.ts";

test("desktop platform decisions are centralized and explicit", () => {
  const macos = capabilitiesFor("macos");
  const linux = capabilitiesFor("linux");
  const windows = capabilitiesFor("windows");

  assert.equal(macos.supportsBiometrics, true);
  assert.equal(windows.supportsBiometrics, true);
  assert.equal(linux.supportsBiometrics, false);

  for (const platform of [macos, linux, windows]) {
    assert.equal(platform.supportsDesktopWindowFeatures, true);
    assert.equal(platform.supportsLocalSecureStorage, true);
    assert.equal(platform.supportsWakeWord, true);
    assert.equal(platform.supportsBackgroundAudio, false);
    assert.equal(platform.supportsNotifications, false);
  }
});

test("unknown runtimes fail closed", () => {
  const unknown = capabilitiesFor("unknown");
  assert.deepEqual(unknown, {
    platform: "unknown",
    supportsBiometrics: false,
    supportsWakeWord: false,
    supportsBackgroundAudio: false,
    supportsNotifications: false,
    supportsLocalSecureStorage: false,
    supportsDesktopWindowFeatures: false,
  });
});
