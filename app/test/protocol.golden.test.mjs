import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  assertEmptySnapshot,
  assertFullSnapshot,
  assertPriorVersionRejected,
  assertSnapshotWithoutTemperatures,
} from "../target/golden/protocol.golden.test.js";

const fixtureDirectory = new URL("../../bridge/target/test-vectors/", import.meta.url);

test("decodes the full Rust version-5 snapshot", async () => {
  assertFullSnapshot(await fixture("snapshot-v5-full.cbor"));
});

test("decodes the Rust version-5 snapshot without temperatures", async () => {
  assertSnapshotWithoutTemperatures(await fixture("snapshot-v5-without-temperatures.cbor"));
});

test("decodes the empty Rust version-5 snapshot", async () => {
  assertEmptySnapshot(await fixture("snapshot-v5-empty.cbor"));
});

test("rejects a prior snapshot version", async () => {
  assertPriorVersionRejected(await fixture("snapshot-v5-full.cbor"));
});

async function fixture(name) {
  return new Uint8Array(await readFile(new URL(name, fixtureDirectory)));
}
