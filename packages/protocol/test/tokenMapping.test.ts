/**
 * Reading a registration diagnostic.
 *
 * The contracts decide which findings apply; this module only names them and gives each one a
 * sentence. What is worth testing here is that a report is read the way both chains write it: an
 * empty list means compatible, the order is the contracts' order and not a sorted one, and a
 * discriminant from a newer contract does not take the page down.
 */
import { describe, expect, it } from "vitest";

import {
  TokenMappingConflict,
  describeConflict,
  explainReport,
  isCompatible,
  type TokenMappingReport,
} from "../src/tokenMapping.js";

const ZERO = "0x0000000000000000000000000000000000000000" as const;
const TOKEN = "0x00000000000000000000000000000000000000aa" as const;
const ID = "0x1111111111111111111111111111111111111111111111111111111111111111" as const;

function report(overrides: Partial<TokenMappingReport> = {}): TokenMappingReport {
  return {
    token: TOKEN,
    tokenId: ID,
    mappedTokenId: ZERO,
    mappedToken: ZERO,
    railToken: TOKEN,
    railManager: ZERO,
    railManagerType: 0,
    managerTypeKnown: false,
    onchainDecimals: 6,
    decimalsKnown: true,
    conflicts: [],
    ...overrides,
  };
}

describe("a registration report", () => {
  it("is compatible when nothing conflicts", () => {
    expect(isCompatible(report())).toBe(true);
  });

  it("is not compatible when anything conflicts", () => {
    expect(isCompatible(report({ conflicts: [TokenMappingConflict.Decimals] }))).toBe(false);
  });

  it("explains every finding, in the order the contract reported them", () => {
    // The order carries meaning: the contracts check the local map before the rail, so a report
    // that listed a rail mismatch above a repointed mapping would tell an operator to fix the
    // wrong thing first.
    const copy = explainReport(
      report({
        conflicts: [
          TokenMappingConflict.TokenId,
          TokenMappingConflict.RailTokenMismatch,
          TokenMappingConflict.Decimals,
        ],
      }),
    );
    expect(copy.map((entry) => entry.label)).toEqual([
      "Asset already linked elsewhere",
      "Token id belongs to another asset",
      "Decimals disagree",
    ]);
  });

  it("says nothing about a compatible report", () => {
    expect(explainReport(report())).toEqual([]);
  });
});

describe("the conflict copy", () => {
  it("has a sentence for every discriminant the contracts can send", () => {
    const named = Object.values(TokenMappingConflict).filter(
      (value) => value !== TokenMappingConflict.None,
    );
    for (const value of named) {
      expect(describeConflict(value).detail.length).toBeGreaterThan(0);
      expect(describeConflict(value).label).not.toBe("Compatible");
    }
  });

  it("does not throw on a discriminant this build has no name for", () => {
    // A contract upgraded ahead of this package is a real deployment ordering, and a diagnostic
    // that throws on the finding it was called to explain is worse than one that shrugs. It must
    // not shrug into "Compatible" either, which would report a refusal as an approval.
    expect(describeConflict(99).label).toBe("Unrecognised finding");
  });

  it("assigns each finding to whoever can actually fix it", () => {
    expect(describeConflict(TokenMappingConflict.TokenId).owner).toBe("local");
    expect(describeConflict(TokenMappingConflict.UnregisteredTokenId).owner).toBe("rail");
    expect(describeConflict(TokenMappingConflict.Decimals).owner).toBe("asset");
  });
});
