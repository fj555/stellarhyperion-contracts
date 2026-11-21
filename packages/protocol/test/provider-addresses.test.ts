import { describe, expect, it } from "vitest";
import {
  MAX_QUOTE_AMOUNT,
  ProviderAddressError,
  QuoteBlocker,
  RouteKind,
  parseProviderAddressOverrides,
  planQuote,
  planQuoteWithProviderAddresses,
  quoteBatchWithProviderAddresses,
  resolveProviderAddressPair,
  type ProviderAwareQuoteRequest,
  type RouterSnapshot,
} from "../src/index.js";
import { C_ADDR, G_ADDR } from "./fixtures.js";

const BASE_SEPOLIA_ADDRESS = "0x1111111111111111111111111111111111111111";
const SEPOLIA_ADDRESS = "0x2222222222222222222222222222222222222222";

const overrides = parseProviderAddressOverrides({
  "base-sepolia": { USDC: { cctp: BASE_SEPOLIA_ADDRESS } },
  sepolia: { USDC: { cctp: SEPOLIA_ADDRESS } },
});

const snapshot: RouterSnapshot = {
  paused: false,
  feeBps: 0,
  routeEnabled: { [RouteKind.Cctp]: true },
  adapterSet: { [RouteKind.Cctp]: true },
  tokenRegistered: true,
  tokenEnabled: true,
  tokenDecimals: 6,
  flowAvailable: { [RouteKind.Cctp]: MAX_QUOTE_AMOUNT },
  chainSupported: { [RouteKind.Cctp]: true },
};

const providerRequest: ProviderAwareQuoteRequest = {
  route: RouteKind.Cctp,
  amount: MAX_QUOTE_AMOUNT,
  strkey: G_ADDR,
  destinationDecimals: 18,
  sourceChain: "base-sepolia",
  destinationChain: "sepolia",
  symbol: "USDC",
  providerAddressOverrides: overrides,
};

describe("provider address overrides", () => {
  it("parses explicit chain, asset, and rail addresses without mutating asset metadata", () => {
    const parsed = parseProviderAddressOverrides({
      "stellar-testnet": { USDC: { cctp: C_ADDR } },
      "base-sepolia": { USDC: { cctp: BASE_SEPOLIA_ADDRESS } },
    });

    expect(parsed["stellar-testnet"]?.USDC?.[RouteKind.Cctp]).toBe(C_ADDR);
    expect(
      resolveProviderAddressPair("stellar-testnet", "base-sepolia", "USDC", RouteKind.Cctp, parsed),
    ).toMatchObject({
      sourceChain: "stellar-testnet",
      destinationChain: "base-sepolia",
      symbol: "USDC",
      source: C_ADDR,
      destination: BASE_SEPOLIA_ADDRESS,
    });
  });

  it("reports malformed external override data at its field", () => {
    for (const [value, path] of [
      [{ unknown: { USDC: { cctp: BASE_SEPOLIA_ADDRESS } } }, "$.unknown"],
      [{ "base-sepolia": { MISSING: { cctp: BASE_SEPOLIA_ADDRESS } } }, "$.base-sepolia.MISSING"],
      [
        { "base-sepolia": { USDC: { wormhole: BASE_SEPOLIA_ADDRESS } } },
        "$.base-sepolia.USDC.wormhole",
      ],
      [{ "base-sepolia": { USDC: { cctp: "not-an-address" } } }, "$.base-sepolia.USDC.cctp"],
      [
        { "base-sepolia": { USDC: { allbridge: BASE_SEPOLIA_ADDRESS } } },
        "$.base-sepolia.USDC.allbridge",
      ],
      [{ "stellar-testnet": { USDC: { cctp: G_ADDR } } }, "$.stellar-testnet.USDC.cctp"],
    ] as const) {
      try {
        parseProviderAddressOverrides(value);
        expect.unreachable();
      } catch (error) {
        expect(error).toBeInstanceOf(ProviderAddressError);
        expect((error as ProviderAddressError).path).toBe(path);
      }
    }
  });

  it("requires a complete provider pair and reports the missing endpoint", () => {
    const sourceOnly = parseProviderAddressOverrides({
      "base-sepolia": { USDC: { cctp: BASE_SEPOLIA_ADDRESS } },
    });
    expect(() =>
      resolveProviderAddressPair("base-sepolia", "sepolia", "USDC", RouteKind.Cctp, sourceOnly),
    ).toThrow("providerAddresses.sepolia.USDC.cctp");
  });

  it("refuses cross-network pairs and provider routes the asset cannot use", () => {
    expect(() =>
      resolveProviderAddressPair("base", "sepolia", "USDC", RouteKind.Cctp, overrides),
    ).toThrow(/same network/);
    expect(() =>
      resolveProviderAddressPair("base-sepolia", "sepolia", "USDC", RouteKind.Allbridge, overrides),
    ).toThrow(/capability is unavailable/);
  });

  it("keeps explicit chain and decimal inputs unchanged at integer boundaries", () => {
    const result = planQuoteWithProviderAddresses(providerRequest, snapshot);
    expect(result.providerAddresses.sourceChain).toBe("base-sepolia");
    expect(result.providerAddresses.destinationChain).toBe("sepolia");
    expect(result.quote).toEqual(
      planQuote(
        {
          route: providerRequest.route,
          amount: MAX_QUOTE_AMOUNT,
          strkey: providerRequest.strkey,
          destinationDecimals: providerRequest.destinationDecimals,
        },
        snapshot,
      ),
    );
    expect(result.quote.available).toBe(true);

    const beyondBoundary = planQuoteWithProviderAddresses(
      { ...providerRequest, amount: MAX_QUOTE_AMOUNT + 1n },
      snapshot,
    );
    expect(beyondBoundary.quote.available).toBe(false);
    expect(beyondBoundary.quote.reason).toBe(QuoteBlocker.NotRepresentable);
  });

  it("returns provider pairs separately from batch quote schemas", () => {
    const result = quoteBatchWithProviderAddresses([MAX_QUOTE_AMOUNT], {
      ...providerRequest,
      snapshot,
    });
    expect(result.providerAddresses[RouteKind.Cctp]?.source).toBe(BASE_SEPOLIA_ADDRESS);
    expect(result.quotes[0]?.amount).toBe(MAX_QUOTE_AMOUNT);
    expect(result.quotes[0]?.quotes).toHaveLength(4);
  });
});
