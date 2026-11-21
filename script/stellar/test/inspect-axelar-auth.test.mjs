import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { resolve, join } from "node:path";
import { readFileSync } from "node:fs";

import {
  inspectAuthSnapshot,
  generateAuthSnapshot,
  parseAuthTreeDebits,
} from "../inspect-axelar-auth.mjs";

const FIXTURES_DIR = resolve("script/stellar/test/fixtures/auth-snapshots");

test("approves valid nested paid dispatch authorization tree matching expected intent", () => {
  const snapshot = JSON.parse(
    readFileSync(join(FIXTURES_DIR, "valid-paid-dispatch", "snapshot.json"), "utf8"),
  );
  const report = inspectAuthSnapshot(snapshot, snapshot.expectedIntent);

  assert.equal(report.safe, true);
  assert.equal(report.status, "approved");
  assert.equal(report.callTreeVerified, true);
  assert.deepEqual(report.privilegeExpansions, []);
  assert.equal(report.authorizedDebits.principal.action, "burn");
  assert.equal(report.authorizedDebits.principal.amount, "99950000");
  assert.equal(report.authorizedDebits.fee.amount, "50000");
  assert.equal(report.authorizedDebits.gas.amount, "1000");
});

test("detects changed spender privilege expansion and rejects before signing", () => {
  const snapshot = JSON.parse(
    readFileSync(join(FIXTURES_DIR, "changed-spender", "snapshot.json"), "utf8"),
  );
  const report = inspectAuthSnapshot(snapshot, snapshot.expectedIntent);

  assert.equal(report.safe, false);
  assert.equal(report.status, "rejected");
  assert.ok(report.privilegeExpansions.some((p) => p.includes("CHANGED_SPENDER")));
  assert.ok(report.warnings.some((w) => w.includes("CHANGED_SPENDER")));
});

test("detects changed gas recipient privilege expansion and rejects before signing", () => {
  const snapshot = JSON.parse(
    readFileSync(join(FIXTURES_DIR, "changed-gas-recipient", "snapshot.json"), "utf8"),
  );
  const report = inspectAuthSnapshot(snapshot, snapshot.expectedIntent);

  assert.equal(report.safe, false);
  assert.equal(report.status, "rejected");
  assert.ok(report.privilegeExpansions.some((p) => p.includes("CHANGED_GAS_RECIPIENT")));
  assert.ok(report.warnings.some((w) => w.includes("CHANGED_GAS_RECIPIENT")));
});

test("detects amount expansion and rejects before signing", () => {
  const snapshot = JSON.parse(
    readFileSync(join(FIXTURES_DIR, "changed-amount", "snapshot.json"), "utf8"),
  );
  const report = inspectAuthSnapshot(snapshot, snapshot.expectedIntent);

  assert.equal(report.safe, false);
  assert.equal(report.status, "rejected");
  assert.ok(report.privilegeExpansions.some((p) => p.includes("AMOUNT_EXPANSION")));
  assert.ok(report.warnings.some((w) => w.includes("AMOUNT_EXPANSION")));
});

test("detects unexpected sub-invocation privilege escalation and rejects before signing", () => {
  const snapshot = JSON.parse(
    readFileSync(join(FIXTURES_DIR, "unexpected-sub-invocation", "snapshot.json"), "utf8"),
  );
  const report = inspectAuthSnapshot(snapshot, snapshot.expectedIntent);

  assert.equal(report.safe, false);
  assert.equal(report.status, "rejected");
  assert.ok(report.privilegeExpansions.some((p) => p.includes("UNEXPECTED_SUB_INVOCATION")));
  assert.ok(report.warnings.some((w) => w.includes("UNEXPECTED_SUB_INVOCATION")));
});

test("detects changed fee recipient and rejects before signing", () => {
  const snapshot = JSON.parse(
    readFileSync(join(FIXTURES_DIR, "changed-fee-recipient", "snapshot.json"), "utf8"),
  );
  const report = inspectAuthSnapshot(snapshot, snapshot.expectedIntent);

  assert.equal(report.safe, false);
  assert.equal(report.status, "rejected");
  assert.ok(report.privilegeExpansions.some((p) => p.includes("CHANGED_FEE_RECIPIENT")));
  assert.ok(report.warnings.some((w) => w.includes("CHANGED_FEE_RECIPIENT")));
});

test("offline snapshot generation creates canonical verifiable structure with no private key or network broadcast", () => {
  const generated = generateAuthSnapshot({
    sender: "GBBD7PQXN7P55U25D2BWWX7AUKC7E3AOP7XGQLB7IYY3Z3QWVQO32N3Q",
    token: "CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75",
    netAmount: "100000000",
    isBurn: true,
    feeRecipient: "GDE34Q275W537R5V2IUVB5P46777EAUCL3G63JZXF6KUS7V6W42C6K4N",
    feeAmount: "50000",
    gasToken: "CGAS7PQXN7P55U25D2BWWX7AUKC7E3AOP7XGQLB7IYY3Z3QWVQO32GAS",
    gasRecipient: "CITS7PQXN7P55U25D2BWWX7AUKC7E3AOP7XGQLB7IYY3Z3QWVQO32ITS",
    gasAmount: "2500",
  });

  assert.equal(generated.sender, "GBBD7PQXN7P55U25D2BWWX7AUKC7E3AOP7XGQLB7IYY3Z3QWVQO32N3Q");
  assert.equal(generated.expectedIntent.netAmount, "100000000");
  assert.equal(generated.expectedIntent.gasAmount, "2500");

  const selfInspection = inspectAuthSnapshot(generated, generated.expectedIntent);
  assert.equal(selfInspection.safe, true);
  assert.equal(selfInspection.status, "approved");
});

test("CLI executes correctly with --fixture and --strict flags", () => {
  const cliPath = resolve("script/stellar/inspect-axelar-auth.mjs");

  // Valid fixture exits 0
  const validFixture = join(FIXTURES_DIR, "valid-paid-dispatch");
  const runValid = spawnSync("node", [cliPath, "--fixture", validFixture, "--strict"], {
    encoding: "utf8",
  });
  assert.equal(runValid.status, 0);
  const parsedValid = JSON.parse(runValid.stdout);
  assert.equal(parsedValid.safe, true);
  assert.equal(parsedValid.status, "approved");

  // Invalid fixture exits 1
  const invalidFixture = join(FIXTURES_DIR, "changed-spender");
  const runInvalid = spawnSync("node", [cliPath, "--fixture", invalidFixture, "--strict"], {
    encoding: "utf8",
  });
  assert.equal(runInvalid.status, 1);
  const parsedInvalid = JSON.parse(runInvalid.stdout);
  assert.equal(parsedInvalid.safe, false);
  assert.equal(parsedInvalid.status, "rejected");
});
