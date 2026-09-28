import { test } from "node:test";
import assert from "node:assert/strict";
import { parseDurationMs } from "../src/duration.ts";

test("units scale to milliseconds", () => {
  assert.equal(parseDurationMs("1500ms"), 1500);
  assert.equal(parseDurationMs("90s"), 90_000);
  assert.equal(parseDurationMs("2m"), 120_000);
});

test("bad input is an error, not a zero", () => {
  for (const bad of ["90", "s", "3d", "99999999999999999999s"]) {
    assert.throws(() => parseDurationMs(bad), undefined, bad);
  }
});
