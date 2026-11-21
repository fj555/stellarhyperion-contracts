#!/usr/bin/env node
import {
  createPublicKey,
  verify as verifySignature,
} from "node:crypto";
import { closeSync, fstatSync, openSync, readSync } from "node:fs";
import { pathToFileURL } from "node:url";

const MAX_FILE_BYTES = 1_048_576;
const MAX_KEY_BYTES = 16_384;
const PROOF_NAMES = [
  "authorization",
  "pool",
  "liquidity",
  "validator",
  "railValidation",
];

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function nonEmptyString(value) {
  return typeof value === "string" && value.trim().length > 0;
}

function isBytes32(value) {
  return typeof value === "string" && /^(?:0x)?[0-9a-f]{64}$/i.test(value);
}

function timestamp(value) {
  if (
    typeof value !== "string" ||
    !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(
      value,
    )
  )
    return null;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function canonicalJson(value) {
  if (value === null || typeof value === "string" || typeof value === "boolean")
    return JSON.stringify(value);
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value))
      throw new Error("Signed evidence must not contain inexact JSON numbers");
    return JSON.stringify(value);
  }
  if (Array.isArray(value))
    return `[${value.map(canonicalJson).join(",")}]`;
  if (isRecord(value)) {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(",")}}`;
  }
  throw new Error("Signed evidence contains an unsupported JSON value");
}

function proofFacts(name, proof) {
  if (!isRecord(proof)) return {};
  if (name === "authorization")
    return {
      routerAddress: proof.routerAddress ?? null,
      adminAddress: proof.adminAddress ?? null,
      dispatchAuthority: proof.dispatchAuthority ?? null,
    };
  if (name === "pool")
    return {
      poolAddress: proof.poolAddress ?? null,
      token: proof.token ?? null,
    };
  if (name === "liquidity")
    return {
      amountAtomic: proof.amountAtomic ?? null,
      token: proof.token ?? null,
    };
  if (name === "validator")
    return {
      trustModel: proof.trustModel ?? null,
      validatorPublicKey: proof.validatorPublicKey ?? null,
      authority: proof.authority ?? null,
    };
  if (name === "railValidation")
    return {
      scope: proof.scope ?? null,
      verifiedBy: proof.verifiedBy ?? null,
    };
  return {};
}

export function acceptancePayload(manifest) {
  if (!isRecord(manifest) || !isRecord(manifest.signedAcceptance))
    throw new Error("Manifest and signedAcceptance object are required");
  const acceptance = Object.fromEntries(
    Object.entries(manifest.signedAcceptance).filter(
      ([key]) => key !== "signatureBase64",
    ),
  );
  return canonicalJson({ ...manifest, signedAcceptance: acceptance });
}

function proofResult(name, proof, now) {
  const result = {
    name,
    status: "missing",
    evidenceReferences: [],
    facts: proofFacts(name, proof),
    issues: [],
  };
  if (!isRecord(proof)) {
    result.issues.push("Required evidence is absent");
    return result;
  }
  if (proof.status === "development-only" || proof.developmentOnly === true) {
    result.status = "development-only";
    result.issues.push("Development-only evidence cannot satisfy activation");
    return result;
  }
  if (proof.status !== "verified") {
    result.status = "missing";
    result.issues.push("Evidence status must be verified");
    return result;
  }

  const references = proof.references;
  if (
    !Array.isArray(references) ||
    references.length === 0 ||
    references.some((reference) => !nonEmptyString(reference))
  )
    result.issues.push("At least one evidence reference is required");
  else result.evidenceReferences = references;

  if (!nonEmptyString(proof.verifiedBy))
    result.issues.push("verifiedBy is required");
  const checkedAt = timestamp(proof.checkedAt);
  const expiresAt = timestamp(proof.expiresAt);
  if (checkedAt === null || checkedAt > now)
    result.issues.push("checkedAt must be a valid, non-future timestamp");
  if (
    expiresAt === null ||
    expiresAt <= now ||
    (checkedAt !== null && expiresAt <= checkedAt)
  )
    result.issues.push("Evidence must have a valid future expiresAt");

  if (result.issues.length === 0) result.status = "verified";
  else result.status = "invalid";
  return result;
}

function candidateResult(candidate) {
  const issues = [];
  if (!isRecord(candidate)) {
    return {
      name: "candidate",
      status: "invalid",
      evidenceReferences: [],
      issues: ["Candidate lane details are required"],
    };
  }
  if (candidate.rail !== "allbridge")
    issues.push("Candidate rail must be allbridge");
  if (!nonEmptyString(candidate.sourceNetwork))
    issues.push("sourceNetwork is required");
  if (candidate.direction !== "outbound")
    issues.push("Only outbound lane candidates are supported");
  if (!nonEmptyString(candidate.destination))
    issues.push("destination is required");
  if (
    !Number.isSafeInteger(candidate.allbridgeChainId) ||
    candidate.allbridgeChainId <= 0 ||
    candidate.allbridgeChainId > 0xffff_ffff ||
    candidate.allbridgeChainId === 7
  )
    issues.push("allbridgeChainId must be a valid non-Stellar u32 chain id");
  if (!nonEmptyString(candidate.localToken))
    issues.push("localToken is required");
  if (!isBytes32(candidate.receiveToken))
    issues.push("receiveToken must be a 32-byte hex value");
  return {
    name: "candidate",
    status: issues.length === 0 ? "verified" : "invalid",
    evidenceReferences: [],
    issues,
  };
}

function checkProofDetails(proof, name, candidate) {
  const issues = [];
  if (name === "authorization") {
    for (const field of ["routerAddress", "adminAddress", "dispatchAuthority"]) {
      if (!nonEmptyString(proof?.[field]))
        issues.push(`${field} is required`);
    }
  } else if (name === "pool") {
    if (!nonEmptyString(proof?.poolAddress))
      issues.push("poolAddress is required");
    if (proof?.token !== candidate?.localToken)
      issues.push("Pool proof token must match candidate.localToken");
  } else if (name === "liquidity") {
    if (
      typeof proof?.amountAtomic !== "string" ||
      !/^[1-9]\d*$/.test(proof.amountAtomic)
    )
      issues.push("amountAtomic must be a positive exact decimal string");
    if (proof?.token !== candidate?.localToken)
      issues.push("Liquidity proof token must match candidate.localToken");
  } else if (name === "validator") {
    if (proof?.trustModel !== "configured-validator")
      issues.push("trustModel must identify Allbridge configured-validator trust");
    if (!isBytes32(proof?.validatorPublicKey))
      issues.push("validatorPublicKey must be a 32-byte hex value");
    if (!nonEmptyString(proof?.authority))
      issues.push("validator authority is required");
  } else if (name === "railValidation") {
    const scope = [
      candidate?.sourceNetwork,
      candidate?.direction,
      candidate?.destination,
    ].join(":");
    if (proof?.scope !== scope)
      issues.push(`Independent validation scope must equal ${scope}`);
  }
  return issues;
}

function acceptanceResult(manifest, trustedKeyPem, acceptorId, now) {
  const result = {
    name: "signedAcceptance",
    status: "missing",
    signer: null,
    issues: [],
  };
  const acceptance = manifest?.signedAcceptance;
  if (!isRecord(acceptance)) {
    result.issues.push("Signed acceptance is required");
    return result;
  }
  result.signer = nonEmptyString(acceptance.signer) ? acceptance.signer : null;
  if (!nonEmptyString(acceptorId) || acceptance.signer !== acceptorId)
    result.issues.push("Acceptance signer must match the externally trusted acceptor id");
  const signedAt = timestamp(acceptance.signedAt);
  if (signedAt === null || signedAt > now)
    result.issues.push("signedAt must be a valid, non-future timestamp");

  let signature;
  if (typeof acceptance.signatureBase64 !== "string") {
    result.issues.push("signatureBase64 is required");
  } else {
    signature = Buffer.from(acceptance.signatureBase64, "base64");
    if (
      signature.length !== 64 ||
      signature.toString("base64") !== acceptance.signatureBase64
    )
      result.issues.push("signatureBase64 must be canonical 64-byte Ed25519 data");
  }

  let publicKey;
  try {
    publicKey = createPublicKey(trustedKeyPem);
    if (publicKey.asymmetricKeyType !== "ed25519")
      result.issues.push("Trusted acceptance key must be Ed25519");
  } catch {
    result.issues.push("Trusted acceptance public key could not be loaded");
  }

  if (signature?.length === 64 && publicKey?.asymmetricKeyType === "ed25519") {
    try {
      if (
        !verifySignature(
          null,
          Buffer.from(acceptancePayload(manifest), "utf8"),
          publicKey,
          signature,
        )
      )
        result.issues.push("Acceptance signature does not match the candidate evidence");
    } catch (error) {
      result.issues.push(error.message);
    }
  }
  if (result.issues.length === 0) result.status = "verified";
  else result.status = "invalid";
  return result;
}

function manifestResult(manifest) {
  const issues = [];
  if (!isRecord(manifest)) issues.push("Manifest must be a JSON object");
  else {
    if (manifest.schemaVersion !== 1)
      issues.push("schemaVersion must be 1");
    if (!isRecord(manifest.evidence))
      issues.push("evidence object is required");
  }
  return {
    name: "manifest",
    status: issues.length === 0 ? "verified" : "invalid",
    evidenceReferences: [],
    issues,
  };
}

export function evaluateActivation(
  manifest,
  { trustedKeyPem, acceptorId, now = Date.now() },
) {
  const candidate = manifest?.candidate;
  const checks = [manifestResult(manifest), candidateResult(candidate)];

  for (const name of PROOF_NAMES) {
    const proof = manifest?.evidence?.[name];
    const result = proofResult(name, proof, now);
    result.issues.push(...checkProofDetails(proof, name, candidate));
    if (result.issues.length > 0 && result.status === "verified")
      result.status = "invalid";
    checks.push(result);
  }
  const railValidation = checks.find(
    (check) => check.name === "railValidation",
  );
  const validationProof = manifest?.evidence?.railValidation;
  if (
    railValidation.status === "verified" &&
    validationProof.verifiedBy === manifest?.signedAcceptance?.signer
  ) {
    railValidation.status = "invalid";
    railValidation.issues.push(
      "Independent validation verifier must differ from the acceptance signer",
    );
  }
  checks.push(acceptanceResult(manifest, trustedKeyPem, acceptorId, now));

  const validator = checks.find((check) => check.name === "validator");
  return {
    schemaVersion: 1,
    candidate: candidate ?? null,
    ready: checks.every((check) => check.status === "verified"),
    decision: checks.every((check) => check.status === "verified")
      ? "evidence-ready-for-independent-approval"
      : "blocked",
    checks,
    trustAssurances: {
      allbridgeValidator: {
        status: validator.status,
        model: "configured-validator",
        publicKey: validator.facts.validatorPublicKey ?? null,
        authority: validator.facts.authority ?? null,
        note: "Allbridge's configured validator key is a distinct trust assumption; it is not validated by Circle or Axelar assurances.",
      },
      circleCctp: {
        status: "not-applicable",
        note: "Circle CCTP attestations do not establish Allbridge validator trust.",
      },
      axelar: {
        status: "not-applicable",
        note: "Axelar validator or ITS assurances do not establish Allbridge validator trust.",
      },
    },
    effects: {
      routeEnabled: false,
      transactionSubmitted: false,
      note: "Read-only evidence evaluation only; a ready result is not deployment or activation authorization.",
    },
    deploymentContext:
      "The active deployment remains Stellar Testnet outbound ITS/CCTP. This report does not activate Allbridge, EVM, or other rail work.",
  };
}

function usage() {
  return [
    "Usage: node script/stellar/check-allbridge-activation.mjs <candidate.json> --trusted-key <acceptor-public-key.pem> --acceptor-id <id>",
    "The trusted acceptance key must be supplied out of band and must be Ed25519.",
  ].join("\n");
}

export function readBoundedFile(file, limit) {
  const fd = openSync(file, "r");
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile()) throw new Error("Input must be a regular file");
    if (stat.size > limit) throw new Error(`Input file exceeds ${limit} bytes`);

    // Read the same descriptor we checked, with one extra byte to detect growth.
    const buffer = Buffer.alloc(limit + 1);
    let length = 0;
    while (length < buffer.length) {
      const bytes = readSync(fd, buffer, length, buffer.length - length, null);
      if (bytes === 0) break;
      length += bytes;
    }
    if (length > limit) throw new Error(`Input file exceeds ${limit} bytes`);
    return buffer.subarray(0, length);
  } finally {
    closeSync(fd);
  }
}

function runCli(args) {
  if (args.length === 1 && args[0] === "--help") {
    console.log(usage());
    return;
  }
  const [manifestPath, ...options] = args;
  let trustedKeyPath;
  let acceptorId;
  while (options.length > 0) {
    const option = options.shift();
    const value = options.shift();
    if (!value) throw new Error(`Missing value for ${option}`);
    if (option === "--trusted-key" && !trustedKeyPath)
      trustedKeyPath = value;
    else if (option === "--acceptor-id" && !acceptorId) acceptorId = value;
    else throw new Error(`Unexpected option: ${option}`);
  }
  if (!manifestPath || !trustedKeyPath || !acceptorId)
    throw new Error(usage());

  const manifest = JSON.parse(readBoundedFile(manifestPath, MAX_FILE_BYTES));
  const keyPem = readBoundedFile(trustedKeyPath, MAX_KEY_BYTES);
  const report = evaluateActivation(manifest, {
    trustedKeyPem: keyPem,
    acceptorId,
  });
  console.log(JSON.stringify(report, null, 2));
  if (!report.ready) process.exitCode = 2;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    runCli(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
