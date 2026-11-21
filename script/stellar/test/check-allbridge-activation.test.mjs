import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import {
  acceptancePayload,
  evaluateActivation,
  readBoundedFile,
} from "../check-allbridge-activation.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(
  HERE,
  "fixtures",
  "allbridge-development-only.json",
);
const NOW = Date.parse("2026-11-20T01:00:00Z");

test("bounded evidence reads reject oversized and non-file inputs", () => {
  const directory = mkdtempSync(path.join(tmpdir(), "allbridge-evidence-"));
  try {
    const file = path.join(directory, "evidence.json");
    writeFileSync(file, "1234");
    assert.equal(readBoundedFile(file, 4).toString(), "1234");
    assert.throws(() => readBoundedFile(file, 3), /exceeds 3 bytes/);
    assert.throws(() => readBoundedFile(directory, 4), /regular file/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

function signedManifest() {
  const manifest = JSON.parse(readFileSync(FIXTURE, "utf8"));
  manifest.evidence.authorization.status = "verified";
  manifest.evidence.authorization.developmentOnly = false;
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  manifest.signedAcceptance = {
    signer: "release-approver",
    signedAt: "2026-11-20T00:30:00Z",
  };
  manifest.signedAcceptance.signatureBase64 = sign(
    null,
    Buffer.from(acceptancePayload(manifest), "utf8"),
    privateKey,
  ).toString("base64");
  return {
    manifest,
    trustedKeyPem: publicKey.export({ type: "spki", format: "pem" }),
    privateKey,
  };
}

test("development-only evidence prevents readiness while trust assumptions stay distinct", () => {
  const report = evaluateActivation(
    JSON.parse(readFileSync(FIXTURE, "utf8")),
    {
      trustedKeyPem: "not used without acceptance",
      acceptorId: "release-approver",
      now: NOW,
    },
  );

  assert.equal(report.ready, false);
  assert.equal(
    report.checks.find((check) => check.name === "authorization").status,
    "development-only",
  );
  assert.equal(report.trustAssurances.allbridgeValidator.model, "configured-validator");
  assert.equal(report.trustAssurances.circleCctp.status, "not-applicable");
  assert.equal(report.trustAssurances.axelar.status, "not-applicable");
  assert.equal(report.effects.routeEnabled, false);
  assert.equal(report.effects.transactionSubmitted, false);
});

test("valid signed evidence is read-only and preserves exact liquidity amounts", () => {
  const { manifest, trustedKeyPem } = signedManifest();
  const report = evaluateActivation(manifest, {
    trustedKeyPem,
    acceptorId: "release-approver",
    now: NOW,
  });

  assert.equal(report.ready, true);
  assert.equal(report.effects.routeEnabled, false);
  assert.equal(report.effects.transactionSubmitted, false);
  assert.equal(manifest.evidence.liquidity.amountAtomic, "9007199254740993");
});

test("signature covers evidence and independent validation has a distinct reviewer", () => {
  const { manifest, trustedKeyPem } = signedManifest();
  manifest.evidence.pool.poolAddress = "changed-after-signing";
  const report = evaluateActivation(manifest, {
    trustedKeyPem,
    acceptorId: "release-approver",
    now: NOW,
  });
  assert.equal(
    report.checks.find((check) => check.name === "signedAcceptance").status,
    "invalid",
  );

  const other = signedManifest();
  other.manifest.evidence.railValidation.verifiedBy = "release-approver";
  other.manifest.signedAcceptance.signatureBase64 = sign(
    null,
    Buffer.from(acceptancePayload(other.manifest), "utf8"),
    other.privateKey,
  ).toString("base64");
  const independentReport = evaluateActivation(other.manifest, {
    trustedKeyPem: other.trustedKeyPem,
    acceptorId: "release-approver",
    now: NOW,
  });
  assert.equal(
    independentReport.checks.find((check) => check.name === "railValidation")
      .status,
    "invalid",
  );
  assert.equal(independentReport.ready, false);

  const impreciseAmount = signedManifest();
  impreciseAmount.manifest.evidence.liquidity.amountAtomic = 9007199254740993;
  const impreciseReport = evaluateActivation(
    impreciseAmount.manifest,
    {
      trustedKeyPem: impreciseAmount.trustedKeyPem,
      acceptorId: "release-approver",
      now: NOW,
    },
  );
  assert.equal(
    impreciseReport.checks.find((check) => check.name === "liquidity").status,
    "invalid",
  );
});
