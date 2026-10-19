import assert from "node:assert/strict";
import test from "node:test";
import { installBlocker, problemLabel, shortHash } from "../src/persona.ts";

const A = "a".repeat(64);
const B = "b".repeat(64);
const version = (sha256) => ({ sha256, bytes: 1, modified: 1, problem: null, installed: false });
const status = (candidate, installed = version(A)) => ({ max_bytes: 32768, installed, candidate, candidate_error: null, history: [] });
const candidate = (overrides = {}) => ({ commit: "0123456789ab", signature: "verified", sha256: B, bytes: 1, problem: null, ...overrides });

test("problem codes map to fixed labels only", () => {
  assert.equal(problemLabel(null), "Valid");
  assert.equal(problemLabel("missing_heading"), "Does not start with a \"# \" heading");
  assert.equal(problemLabel("unrecognized"), "Refused by validation");
  assert.equal(shortHash(B), "bbbbbbbbbbbb");
});

test("only a verified, valid and different candidate can be installed", () => {
  assert.equal(installBlocker(status(candidate())), null);
  assert.equal(installBlocker(status(candidate(), null)), null);
  assert.match(installBlocker(status(candidate({ signature: "untrusted" }))), /not signed/);
  assert.match(installBlocker(status(candidate({ signature: "no_allowed_signers" }))), /not signed/);
  assert.match(installBlocker(status(candidate({ sha256: null, bytes: null, problem: "too_large" }))), /size limit/);
  assert.match(installBlocker(status(candidate({ problem: "secret_like" }))), /key or token/);
  assert.match(installBlocker(status(candidate({ sha256: A }))), /already installed/);
  assert.equal(installBlocker({ ...status(null), candidate_error: "private agent checkout is unavailable" }), "private agent checkout is unavailable");
  assert.match(installBlocker(status(null)), /No candidate/);
});
