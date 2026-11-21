import { describe, expect, it } from "vitest";
import {
  compareSnapshots,
  MAX_QUOTE_AMOUNT,
  SnapshotStatus,
  type DatedSnapshot,
  type RouterSnapshot,
} from "../src/quotes.js";
import {
  adapterFor,
  deployedChains,
  deploymentFor,
  isEvmDeployment,
  isStellarDeployment,
  tokenFor,
} from "../src/deployments.js";
import { ROUTE_KINDS, RouteKind } from "../src/routes.js";
import { C_ADDR, G_ADDR } from "./fixtures.js";

const source = "0x1111111111111111111111111111111111111111";
const representation = {
  tokenDecimals: 6,
  destinationNetwork: "stellar-testnet",
  destinationTokenId: C_ADDR,
  destinationDecimals: 7,
};
const reference = { network: "sepolia", tokenId: source, now: 1000, representation };
const request = { amount: 1_000_000n, strkey: G_ADDR, destinationDecimals: 7 };
function state(overrides: Partial<RouterSnapshot> = {}): RouterSnapshot {
  const flags = Object.fromEntries(ROUTE_KINDS.map((route) => [route, true]));
  return {
    paused: false,
    feeBps: 10,
    routeEnabled: flags,
    adapterSet: flags,
    chainSupported: flags,
    tokenRegistered: true,
    tokenEnabled: true,
    tokenDecimals: 6,
    flowAvailable: Object.fromEntries(ROUTE_KINDS.map((route) => [route, MAX_QUOTE_AMOUNT])),
    ...overrides,
  };
}
function entry(overrides: Partial<DatedSnapshot> = {}): DatedSnapshot {
  return {
    id: "a",
    takenAt: 990,
    network: "sepolia",
    tokenId: source,
    representation,
    snapshot: state(),
    ...overrides,
  };
}
function compare(items: readonly DatedSnapshot[]) {
  return compareSnapshots(request, items, reference, 10);
}
describe("dated snapshot comparison", () => {
  it("ranks all rails in enum order and resolves ties independently of snapshot order", () => {
    const items = [entry({ id: "z" }), entry({ id: "b", takenAt: 995 }), entry({ id: "a" })];
    const output = compare(items);
    expect(output.map((row) => row.snapshotId)).toEqual(["a", "z", "b"]);
    expect(compare([...items].reverse())).toEqual(output);
    for (const row of output) {
      expect(row.status).toBe(SnapshotStatus.Fresh);
      expect(row.quotes.map((q) => q.route)).toEqual(ROUTE_KINDS);
      // CCTP and ITS both await attestation; enum order resolves this complete tie.
      expect(row.quote!.route).toBe(RouteKind.Cctp);
      expect(row.planningOnly).toBe(true);
    }
  });
  it("preserves the original optional single-route request while reporting the complete ladder", () => {
    const row = compareSnapshots(
      { ...request, route: RouteKind.Cctp },
      [entry()],
      reference,
      10,
    )[0]!;
    expect(row.quote!.route).toBe(RouteKind.Cctp);
    expect(row.quotes).toHaveLength(4);
  });
  it("keeps unavailable routes distinct from stale input and accepts the exact age boundary", () => {
    expect(compare([entry()])[0]!.status).toBe(SnapshotStatus.Fresh);
    expect(compare([entry({ snapshot: state({ paused: true }) })])[0]!.status).toBe(
      SnapshotStatus.Unavailable,
    );
    expect(compare([entry({ takenAt: 989, snapshot: state({ paused: true }) })])[0]!.status).toBe(
      SnapshotStatus.Stale,
    );
    expect(compare([entry({ takenAt: 1001 })])[0]!.status).toBe(SnapshotStatus.Future);
  });
  it.each([NaN, Infinity, -1, 1.5, Number.MAX_SAFE_INTEGER + 1])(
    "rejects invalid observation seconds %s",
    (takenAt) => {
      expect(compare([entry({ takenAt })])[0]!.status).toBe(SnapshotStatus.Invalid);
    },
  );
  it.each([NaN, Infinity, -1, 1.5, Number.MAX_SAFE_INTEGER + 1])(
    "rejects invalid reference clock or age %s",
    (value) => {
      expect(() => compareSnapshots(request, [entry()], { ...reference, now: value }, 10)).toThrow(
        RangeError,
      );
      expect(() => compareSnapshots(request, [entry()], reference, value)).toThrow(RangeError);
    },
  );
  it("rejects ambiguous or missing IDs without choosing one by input order", () => {
    expect(
      compare([entry(), entry()]).every(
        (row) => row.status === SnapshotStatus.Invalid && row.quote === null,
      ),
    ).toBe(true);
    expect(compare([entry({ id: " " })])[0]!.status).toBe(SnapshotStatus.Invalid);
    expect(compare([null as unknown as DatedSnapshot])[0]!.status).toBe(SnapshotStatus.Invalid);
  });
  it("does not equate ticker labels, different networks, token contracts or precisions", () => {
    const mismatches = [
      entry({ representation: undefined }),
      entry({ network: "ethereum" }),
      entry({ tokenId: "USDC" }),
      entry({ tokenId: "0x2222222222222222222222222222222222222222" }),
      entry({ tokenId: `0x${"0".repeat(40)}` }),
      entry({ representation: { ...representation, tokenDecimals: 7 } }),
      entry({ representation: { ...representation, destinationNetwork: "stellar" } }),
      entry({ representation: { ...representation, destinationTokenId: G_ADDR } }),
      entry({ snapshot: state({ tokenDecimals: 7 }) }),
      entry({ representation: { ...representation, destinationDecimals: 6 } }),
    ];
    for (const item of mismatches)
      expect(compare([item])[0]!.status).toBe(SnapshotStatus.Incompatible);
  });
  it("refuses unsupported directions even when both snapshot identities agree", () => {
    const directions = [
      {
        network: "sepolia",
        tokenId: source,
        representation: {
          ...representation,
          destinationNetwork: "base-sepolia",
          destinationTokenId: source,
        },
      },
      {
        network: "stellar-testnet",
        tokenId: C_ADDR,
        representation: {
          ...representation,
          destinationNetwork: "sepolia",
          destinationTokenId: source,
        },
      },
      { network: "stellar-testnet", tokenId: C_ADDR, representation },
    ];
    for (const direction of directions) {
      const row = compareSnapshots(
        request,
        [entry(direction)],
        { ...reference, ...direction },
        10,
      )[0]!;
      expect(row.status).toBe(SnapshotStatus.Incompatible);
      expect(row.quote).toBeNull();
      expect(row.quotes).toEqual([]);
    }
  });
  it("normalizes EVM address case while keeping chain and denomination bound", () => {
    const tokenId = "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd";
    const a = entry({ tokenId: tokenId.toUpperCase().replace("0X", "0x") });
    const row = compareSnapshots(request, [a], { ...reference, tokenId }, 10)[0]!;
    expect(row.status).toBe(SnapshotStatus.Fresh);
  });
  it("reports malformed externally supplied router state instead of calculating from it", () => {
    const malformed = [
      null,
      state({ feeBps: NaN }),
      state({ feeBps: -1 }),
      state({ feeBps: 101 }),
      state({ feeBps: 1.5 }),
      state({ flowAvailable: { [RouteKind.Cctp]: -1n } }),
      { ...state(), routeEnabled: null },
      { ...state(), flowAvailable: { [RouteKind.Cctp]: "1" } },
    ];
    for (const snapshot of malformed) {
      expect(compare([entry({ snapshot: snapshot as unknown as RouterSnapshot })])[0]!.status).toBe(
        SnapshotStatus.Invalid,
      );
    }
  });
  it("retains bigint precision beyond the JavaScript number range and refuses overflowing requests", () => {
    const amount = 9_007_199_254_740_993n;
    const row = compareSnapshots({ ...request, amount }, [entry()], reference, 10)[0]!;
    expect(row.quote!.fee).toBe((amount * 10n) / 10000n);
    expect(row.quote!.destinationAmount).toBe((amount - (amount * 10n) / 10000n) * 10n);
    expect(
      compareSnapshots({ ...request, amount: MAX_QUOTE_AMOUNT + 1n }, [entry()], reference, 10)[0]!
        .status,
    ).toBe(SnapshotStatus.Unavailable);
    expect(compareSnapshots({ ...request, amount: 0n }, [entry()], reference, 10)[0]!.status).toBe(
      SnapshotStatus.Unavailable,
    );
  });
  it("preserves every established deployment lookup export", () => {
    for (const lookup of [
      adapterFor,
      deployedChains,
      deploymentFor,
      isEvmDeployment,
      isStellarDeployment,
      tokenFor,
    ])
      expect(typeof lookup).toBe("function");
  });
});
