/**
 * Pricing a route without asking the chain.
 *
 * The router has a `quote` that never reverts, which is the honest answer and the one an app
 * should show before anybody signs. This is the same ladder, in the same order, running locally.
 * It exists for one reason: a planner that has to render four rails while somebody is still
 * typing cannot do four `eth_call`s per keystroke, and a number that appears half a second after
 * the digit that produced it feels broken even when it is right.
 *
 * So the app prices locally as you type and confirms on chain before you sign. The two have to
 * agree, and `quote.test.ts` walks the same cases through both. Where they ever disagree the
 * chain is right and this file is a bug.
 */
import {
  BPS_DENOMINATOR,
  MAX_DECIMALS,
  MAX_FEE_BPS,
  convertDecimals,
  floorToRepresentable,
  pow10,
} from "./amounts.js";
import { tryParseStellarAddress, AddressKind } from "./addresses.js";
import type { RouteKind } from "./routes.js";
import {
  ROUTE_KINDS,
  carriesPayload,
  isCanonical,
  isRouteKind,
  waitsOnAttestation,
} from "./routes.js";
import {
  resolveProviderAddressPair,
  type ProviderAddressOverrides,
  type ProviderAddressPair,
} from "./registry/assets.js";
import type { ChainKey } from "./chains.js";

/**
 * Why a rail turned a quote down. Same integers as the Solidity enum and the Soroban one, in
 * declaration order, because these values travel in events an indexer reads on both chains.
 */
export const QuoteBlocker = {
  None: 0,
  Paused: 1,
  RouteDisabled: 2,
  AdapterNotSet: 3,
  TokenNotRegistered: 4,
  TokenDisabled: 5,
  AmountTooSmall: 6,
  FlowLimitExceeded: 7,
  NotRepresentable: 8,
  MuxedNotSupported: 9,
  InvalidDestination: 10,
  ChainNotSupported: 11,
} as const;

export type QuoteBlocker = (typeof QuoteBlocker)[keyof typeof QuoteBlocker];

/** Whose problem a blocker is, which decides whether the app offers a fix or an apology. */
export type BlockerOwner = "sender" | "operator" | "rail";

export interface BlockerCopy {
  /** Short enough for a row in a route list. */
  readonly label: string;
  /** One sentence, written for somebody who did not read the architecture document. */
  readonly detail: string;
  readonly owner: BlockerOwner;
  /** Whether trying the same thing again later could plausibly work. */
  readonly transient: boolean;
}

const COPY: Record<QuoteBlocker, BlockerCopy> = {
  [QuoteBlocker.None]: {
    label: "Ready",
    detail: "This rail can carry the transfer right now.",
    owner: "sender",
    transient: false,
  },
  [QuoteBlocker.Paused]: {
    label: "Departures paused",
    detail:
      "Somebody pulled the brake on this router. Arrivals still work, so anything already in flight keeps moving.",
    owner: "operator",
    transient: true,
  },
  [QuoteBlocker.RouteDisabled]: {
    label: "Rail switched off",
    detail: "This rail is turned off here. The other rails in the list are unaffected.",
    owner: "operator",
    transient: true,
  },
  [QuoteBlocker.AdapterNotSet]: {
    label: "Rail not wired up",
    detail: "No adapter is registered for this rail on this chain yet.",
    owner: "operator",
    transient: false,
  },
  [QuoteBlocker.TokenNotRegistered]: {
    label: "Asset not listed",
    detail: "This router has never been told about this asset, so it will not move it.",
    owner: "operator",
    transient: false,
  },
  [QuoteBlocker.TokenDisabled]: {
    label: "Asset retired",
    detail: "This asset was registered once and has since been retired here.",
    owner: "operator",
    transient: false,
  },
  [QuoteBlocker.AmountTooSmall]: {
    label: "Amount too small",
    detail: "After the fee there would be nothing left to send.",
    owner: "sender",
    transient: false,
  },
  [QuoteBlocker.FlowLimitExceeded]: {
    label: "Over the window limit",
    detail:
      "This would push the amount crossing in the current window past its ceiling. Send less, or wait for the window to roll.",
    owner: "sender",
    transient: true,
  },
  [QuoteBlocker.NotRepresentable]: {
    label: "Precision would be lost",
    detail:
      "The destination keeps fewer decimal places than this amount needs, and rounding somebody's money down without saying so is not an option.",
    owner: "sender",
    transient: false,
  },
  [QuoteBlocker.MuxedNotSupported]: {
    label: "Muxed address needs a payload",
    detail:
      "This rail cannot carry the sub account id an M address depends on. Use a rail that carries a payload, or the underlying G address.",
    owner: "sender",
    transient: false,
  },
  [QuoteBlocker.InvalidDestination]: {
    label: "Address does not check out",
    detail:
      "That is not a Stellar address, or a character in it changed somewhere between the wallet and here.",
    owner: "sender",
    transient: false,
  },
  [QuoteBlocker.ChainNotSupported]: {
    label: "Rail does not reach there",
    detail: "This rail has no route to that chain, at least not one configured here.",
    owner: "rail",
    transient: false,
  },
};

export const BLOCKER_COPY: Readonly<Record<QuoteBlocker, BlockerCopy>> = Object.freeze(COPY);

export function describeBlocker(blocker: QuoteBlocker): BlockerCopy {
  return COPY[blocker];
}

/**
 * The widest amount the router will price.
 *
 * `quote` refuses anything past this rather than risking an overflow inside a function whose
 * whole promise is that it answers instead of reverting. Nothing anybody is actually sending
 * comes near it.
 */
export const MAX_QUOTE_AMOUNT = (1n << 128n) - 1n;

/** What a route would do with an amount right now, without doing it. */
export interface RouteQuote {
  readonly route: RouteKind;
  readonly available: boolean;
  readonly reason: QuoteBlocker;
  /** Fee plus net, which is what leaves the sender's wallet. */
  readonly grossAmount: bigint;
  readonly fee: bigint;
  readonly netAmount: bigint;
  /** What lands, in the destination's own decimal base. */
  readonly destinationAmount: bigint;
  readonly flowAvailable: bigint;
  readonly waitsOnAttestation: boolean;
  readonly isCanonical: boolean;
}

/**
 * The router state a local quote needs.
 *
 * All of it is readable in one multicall, and the app refreshes it on a block rather than on a
 * keystroke. Keeping it in one object rather than a dozen arguments means adding a check later is
 * a field, not a signature change across every caller.
 */
export interface RouterSnapshot {
  readonly paused: boolean;
  readonly feeBps: number;
  /** Per rail. A rail missing from the record counts as disabled. */
  readonly routeEnabled: Partial<Record<RouteKind, boolean>>;
  /** Per rail. False when no adapter is registered. */
  readonly adapterSet: Partial<Record<RouteKind, boolean>>;
  readonly tokenRegistered: boolean;
  readonly tokenEnabled: boolean;
  readonly tokenDecimals: number;
  /** Per rail, in this chain's units for this asset. */
  readonly flowAvailable: Partial<Record<RouteKind, bigint>>;
  /** Per rail, whether the adapter says it can reach the destination chain. */
  readonly chainSupported: Partial<Record<RouteKind, boolean>>;
}

export interface QuoteRequest {
  readonly route: RouteKind;
  readonly amount: bigint;
  /** The destination address, exactly as it was pasted. */
  readonly strkey: string;
  readonly destinationDecimals: number;
}

export interface ForwardingBudget {
  /** Amount reserved from the bridged destination token before checking the recipient minimum. */
  readonly destinationAmount: bigint;
  readonly unit: "destination-token-base-units";
}

export interface ForwardQuoteRequest extends QuoteRequest {
  readonly minimumDestinationAmount: bigint;
  readonly forwardingBudget: ForwardingBudget;
}

export interface ForwardQuotePlan {
  readonly available: boolean;
  readonly meetsMinimum: boolean;
  readonly reason: string | null;
  readonly quote: RouteQuote;
  readonly recipientDestinationAmount: bigint;
  readonly minimumDestinationAmount: bigint;
  readonly forwardingBudget: ForwardingBudget;
  readonly roundingOvershoot: bigint | null;
  readonly roundingBoundDestinationAmount: bigint;
}

export interface InverseQuoteRequest extends Omit<QuoteRequest, "amount"> {
  readonly minimumDestinationAmount: bigint;
  readonly forwardingBudget: ForwardingBudget;
}

export interface InverseQuotePlan {
  readonly available: boolean;
  readonly reason: string | null;
  readonly blocker: QuoteBlocker | null;
  readonly requiredSourceAmount: bigint | null;
  readonly requiredDestinationAmount: bigint;
  readonly recipientDestinationAmount: bigint;
  readonly minimumDestinationAmount: bigint;
  readonly forwardingBudget: ForwardingBudget;
  readonly roundingOvershoot: bigint | null;
  /** Largest overshoot possible from one source-base-unit rounding step. */
  readonly roundingBoundDestinationAmount: bigint;
  readonly quote: RouteQuote | null;
}

/** Explicit provider context for callers that need provider-side token addresses with a quote. */
export interface ProviderAwareQuoteRequest extends QuoteRequest {
  readonly sourceChain: ChainKey;
  readonly destinationChain: ChainKey;
  readonly symbol: string;
  readonly providerAddressOverrides: ProviderAddressOverrides;
}

export interface ProviderAwareQuoteResult {
  readonly quote: RouteQuote;
  readonly providerAddresses: ProviderAddressPair;
}

/**
 * Quote one route while resolving both explicit provider addresses.
 *
 * Chain and precision continue to come from the request and snapshot; provider addresses are
 * metadata only and cannot silently rewrite either.
 */
export function planQuoteWithProviderAddresses(
  request: ProviderAwareQuoteRequest,
  snapshot: RouterSnapshot,
): ProviderAwareQuoteResult {
  const providerAddresses = resolveProviderAddressPair(
    request.sourceChain,
    request.destinationChain,
    request.symbol,
    request.route,
    request.providerAddressOverrides,
  );
  return {
    quote: planQuote(request, snapshot),
    providerAddresses,
  };
}

function blocked(base: RouteQuote, reason: QuoteBlocker): RouteQuote {
  return { ...base, available: false, reason };
}

function destinationRoundingBound(sourceDecimals: number, destinationDecimals: number): bigint {
  return destinationDecimals > sourceDecimals
    ? pow10(destinationDecimals - sourceDecimals) - 1n
    : 0n;
}

/**
 * Price one rail. Never throws, for the same reason the on-chain version never reverts: an app
 * rendering four rows should not have to wrap each one in a try, and even the absurd amount
 * deserves a reason rather than a stack trace.
 */
export function planQuote(request: QuoteRequest, snapshot: RouterSnapshot): RouteQuote {
  const { route, amount, destinationDecimals } = request;
  const base: RouteQuote = {
    route,
    available: false,
    reason: QuoteBlocker.None,
    grossAmount: amount,
    fee: 0n,
    netAmount: 0n,
    destinationAmount: 0n,
    flowAvailable: 0n,
    waitsOnAttestation: waitsOnAttestation(route),
    isCanonical: isCanonical(route),
  };

  // The half of a quote the amount has nothing to do with, in the router's own order.
  if (snapshot.paused) return blocked(base, QuoteBlocker.Paused);
  if (snapshot.routeEnabled[route] !== true) return blocked(base, QuoteBlocker.RouteDisabled);
  if (snapshot.adapterSet[route] !== true) return blocked(base, QuoteBlocker.AdapterNotSet);

  const parsed = tryParseStellarAddress(request.strkey);
  if (!parsed.ok) return blocked(base, QuoteBlocker.InvalidDestination);
  if (parsed.parts.kind === AddressKind.MuxedAccount && !carriesPayload(route)) {
    return blocked(base, QuoteBlocker.MuxedNotSupported);
  }
  if (snapshot.chainSupported[route] !== true) return blocked(base, QuoteBlocker.ChainNotSupported);

  if (!snapshot.tokenRegistered) return blocked(base, QuoteBlocker.TokenNotRegistered);
  if (!snapshot.tokenEnabled) return blocked(base, QuoteBlocker.TokenDisabled);

  if (amount <= 0n) return blocked(base, QuoteBlocker.AmountTooSmall);
  if (amount > MAX_QUOTE_AMOUNT) return blocked(base, QuoteBlocker.NotRepresentable);

  const fee = (amount * BigInt(snapshot.feeBps)) / BPS_DENOMINATOR;
  const netRaw = amount - fee;
  if (netRaw === 0n) return blocked(base, QuoteBlocker.AmountTooSmall);
  if (snapshot.tokenDecimals > MAX_DECIMALS || destinationDecimals > MAX_DECIMALS) {
    return blocked(base, QuoteBlocker.NotRepresentable);
  }

  const net = floorToRepresentable(netRaw, snapshot.tokenDecimals, destinationDecimals);
  if (net === 0n) return blocked(base, QuoteBlocker.NotRepresentable);

  const headroom = snapshot.flowAvailable[route] ?? 0n;
  const withFlow: RouteQuote = { ...base, flowAvailable: headroom };
  if (net > headroom) return blocked(withFlow, QuoteBlocker.FlowLimitExceeded);

  const { converted } = convertDecimals(net, snapshot.tokenDecimals, destinationDecimals);
  return {
    ...withFlow,
    available: true,
    reason: QuoteBlocker.None,
    fee,
    netAmount: net,
    grossAmount: fee + net,
    destinationAmount: converted,
  };
}

/**
 * Apply the caller's explicit destination-token reserve to a forward quote.
 *
 * This is an amount budget, not a gas-price conversion: it is reserved in destination token
 * base units and does not model native-token provider pricing.
 */
function validForwardingBudget(value: unknown): value is ForwardingBudget {
  if (
    typeof value !== "object" ||
    value === null ||
    !("unit" in value) ||
    !("destinationAmount" in value)
  )
    return false;
  return (
    value.unit === "destination-token-base-units" &&
    typeof value.destinationAmount === "bigint" &&
    value.destinationAmount >= 0n
  );
}

export function planForwardQuote(
  request: ForwardQuoteRequest,
  snapshot: RouterSnapshot,
): ForwardQuotePlan {
  const { minimumDestinationAmount, forwardingBudget } = request;
  const quote = planQuote(request, snapshot);
  const roundingBoundDestinationAmount =
    Number.isInteger(snapshot.tokenDecimals) &&
    Number.isInteger(request.destinationDecimals) &&
    snapshot.tokenDecimals >= 0 &&
    request.destinationDecimals >= snapshot.tokenDecimals &&
    request.destinationDecimals <= MAX_DECIMALS
      ? destinationRoundingBound(snapshot.tokenDecimals, request.destinationDecimals)
      : 0n;
  const validInputs =
    typeof minimumDestinationAmount === "bigint" &&
    minimumDestinationAmount > 0n &&
    validForwardingBudget(forwardingBudget);

  if (!validInputs) {
    return {
      available: false,
      meetsMinimum: false,
      reason:
        "minimum must be positive and forwarding budget non-negative, both in destination-token-base-units",
      quote,
      recipientDestinationAmount: 0n,
      minimumDestinationAmount,
      forwardingBudget,
      roundingOvershoot: null,
      roundingBoundDestinationAmount,
    };
  }

  const recipientDestinationAmount =
    quote.destinationAmount > forwardingBudget.destinationAmount
      ? quote.destinationAmount - forwardingBudget.destinationAmount
      : 0n;
  const available = quote.available;
  const meetsMinimum = available && recipientDestinationAmount >= minimumDestinationAmount;

  return {
    available,
    meetsMinimum,
    reason: !available
      ? BLOCKER_COPY[quote.reason].detail
      : meetsMinimum
        ? null
        : "quoted destination amount after the forwarding reserve is below the requested minimum",
    quote,
    recipientDestinationAmount,
    minimumDestinationAmount,
    forwardingBudget,
    roundingOvershoot: meetsMinimum ? recipientDestinationAmount - minimumDestinationAmount : null,
    roundingBoundDestinationAmount,
  };
}

/**
 * Find the smallest source amount whose exact forward quote meets a destination minimum.
 *
 * Search is bounded by the router quote ceiling and the route's current flow headroom. It uses
 * the same fee floor and destination-precision floor as `planQuote`; no floating-point estimate
 * or guessed fee inversion is involved.
 */
export function planInverseQuote(
  request: InverseQuoteRequest,
  snapshot: RouterSnapshot,
): InverseQuotePlan {
  const { route, strkey, destinationDecimals, minimumDestinationAmount, forwardingBudget } =
    request;
  const initialRoundingBoundDestinationAmount =
    Number.isInteger(snapshot.tokenDecimals) &&
    Number.isInteger(destinationDecimals) &&
    snapshot.tokenDecimals >= 0 &&
    destinationDecimals >= snapshot.tokenDecimals &&
    destinationDecimals <= MAX_DECIMALS
      ? destinationRoundingBound(snapshot.tokenDecimals, destinationDecimals)
      : 0n;
  const requiredDestinationAmount =
    typeof minimumDestinationAmount === "bigint" && validForwardingBudget(forwardingBudget)
      ? minimumDestinationAmount + forwardingBudget.destinationAmount
      : 0n;

  const failure = (reason: string, blocker: QuoteBlocker | null = null): InverseQuotePlan => ({
    available: false,
    reason,
    blocker,
    requiredSourceAmount: null,
    requiredDestinationAmount,
    recipientDestinationAmount: 0n,
    minimumDestinationAmount,
    forwardingBudget,
    roundingOvershoot: null,
    roundingBoundDestinationAmount: initialRoundingBoundDestinationAmount,
    quote: null,
  });

  if (
    typeof minimumDestinationAmount !== "bigint" ||
    minimumDestinationAmount <= 0n ||
    !validForwardingBudget(forwardingBudget)
  ) {
    return failure(
      "minimum must be positive and forwarding budget non-negative, both in destination-token-base-units",
    );
  }
  if (!isRouteKind(route)) return failure("route is not a recognized rail");
  if (!Number.isInteger(snapshot.feeBps) || snapshot.feeBps < 0 || snapshot.feeBps > MAX_FEE_BPS) {
    return failure(`fee assumption must be an integer from 0 to ${String(MAX_FEE_BPS)} bps`);
  }
  if (
    !Number.isInteger(snapshot.tokenDecimals) ||
    snapshot.tokenDecimals < 0 ||
    snapshot.tokenDecimals > MAX_DECIMALS ||
    !Number.isInteger(destinationDecimals) ||
    destinationDecimals < 0 ||
    destinationDecimals > MAX_DECIMALS
  ) {
    return failure(
      `source and destination decimals must be integers from 0 to ${String(MAX_DECIMALS)}`,
    );
  }

  const firstQuote = planQuote({ route, amount: 1n, strkey, destinationDecimals }, snapshot);
  if (
    firstQuote.reason !== QuoteBlocker.None &&
    firstQuote.reason !== QuoteBlocker.AmountTooSmall &&
    firstQuote.reason !== QuoteBlocker.NotRepresentable
  ) {
    return failure(BLOCKER_COPY[firstQuote.reason].detail, firstQuote.reason);
  }

  const flowHeadroom = snapshot.flowAvailable[route] ?? 0n;
  const feeBps = BigInt(snapshot.feeBps);
  let low = 0n;
  let high = MAX_QUOTE_AMOUNT;
  while (low < high) {
    const middle = (low + high + 1n) >> 1n;
    const fee = (middle * feeBps) / BPS_DENOMINATOR;
    const net = floorToRepresentable(middle - fee, snapshot.tokenDecimals, destinationDecimals);
    if (net <= flowHeadroom) low = middle;
    else high = middle - 1n;
  }
  const maximumSourceAmount = low;
  if (maximumSourceAmount === 0n) {
    return failure(
      "route flow headroom is zero or too small for a representable transfer",
      QuoteBlocker.FlowLimitExceeded,
    );
  }

  const maximumQuote = planQuote(
    { route, amount: maximumSourceAmount, strkey, destinationDecimals },
    snapshot,
  );
  if (!maximumQuote.available) {
    return failure(BLOCKER_COPY[maximumQuote.reason].detail, maximumQuote.reason);
  }
  if (maximumQuote.destinationAmount < requiredDestinationAmount) {
    return failure(
      maximumSourceAmount === MAX_QUOTE_AMOUNT
        ? "minimum is not representable within the router's maximum quote amount"
        : "minimum is unattainable within the route's current flow headroom",
      maximumSourceAmount === MAX_QUOTE_AMOUNT
        ? QuoteBlocker.NotRepresentable
        : QuoteBlocker.FlowLimitExceeded,
    );
  }

  let searchLow = 1n;
  let searchHigh = maximumSourceAmount;
  while (searchLow < searchHigh) {
    const middle = (searchLow + searchHigh) >> 1n;
    const quote = planQuote({ route, amount: middle, strkey, destinationDecimals }, snapshot);
    if (quote.available && quote.destinationAmount >= requiredDestinationAmount) {
      searchHigh = middle;
    } else {
      searchLow = middle + 1n;
    }
  }

  const quote = planQuote({ route, amount: searchLow, strkey, destinationDecimals }, snapshot);
  if (!quote.available) {
    return failure(BLOCKER_COPY[quote.reason].detail, quote.reason);
  }
  const recipientDestinationAmount = quote.destinationAmount - forwardingBudget.destinationAmount;
  const roundingOvershoot = recipientDestinationAmount - minimumDestinationAmount;

  return {
    available: true,
    reason: null,
    blocker: null,
    requiredSourceAmount: searchLow,
    requiredDestinationAmount,
    recipientDestinationAmount,
    minimumDestinationAmount,
    forwardingBudget,
    roundingOvershoot,
    roundingBoundDestinationAmount: destinationRoundingBound(
      snapshot.tokenDecimals,
      destinationDecimals,
    ),
    quote,
  };
}

/** Price every rail, in the enum's own order so a list does not reshuffle itself between renders. */
export function planQuotes(
  request: Omit<QuoteRequest, "route">,
  snapshot: RouterSnapshot,
): readonly RouteQuote[] {
  return ROUTE_KINDS.map((route) => planQuote({ ...request, route }, snapshot));
}

/**
 * Which rail to put at the top.
 *
 * Most arriving wins, because that is the number people actually compare. Ties break towards the
 * canonical asset over a wrapper of it, then towards the rail that does not make somebody wait on
 * an attestation. Nothing here is clever, which is deliberate: a recommendation people cannot
 * explain to themselves is a recommendation they are right not to trust.
 */
export function bestQuote(quotes: readonly RouteQuote[]): RouteQuote | null {
  const usable = quotes.filter((q) => q.available);
  if (usable.length === 0) return null;
  return usable.reduce((best, candidate) => (outranks(candidate, best) ? candidate : best));
}

function outranks(candidate: RouteQuote, incumbent: RouteQuote): boolean {
  if (candidate.destinationAmount !== incumbent.destinationAmount) {
    return candidate.destinationAmount > incumbent.destinationAmount;
  }
  if (candidate.isCanonical !== incumbent.isCanonical) return candidate.isCanonical;
  if (candidate.waitsOnAttestation !== incumbent.waitsOnAttestation) {
    return !candidate.waitsOnAttestation;
  }
  return false;
}

/**
 * Whether a quote would trip the router's slippage guard.
 *
 * Deliberately not part of `planQuote`, because the router's `quote` does not know about a
 * minimum either. `bridgeOut` is where `minDestinationAmount` is enforced, and it reverts there
 * with `SlippageExceeded`. This is the courtesy check that stops an app letting somebody sign a
 * transaction it could already tell was going to fail.
 */
export function wouldSlip(quote: RouteQuote, minDestinationAmount: bigint): boolean {
  return quote.available && quote.destinationAmount < minDestinationAmount;
}

export * from "./quotes/snapshots.js";

/**
 * Rounds the floor down, so the guard is never tighter than the tolerance somebody chose. On a
 * rail with a fixed rate this is theatre, and on one that skims a variable fee it is the
 * difference between a transfer and a surprise.
 */
export function minimumFromTolerance(quote: RouteQuote, toleranceBps: number): bigint {
  if (toleranceBps < 0 || toleranceBps > Number(BPS_DENOMINATOR)) {
    throw new RangeError(`tolerance out of range: ${String(toleranceBps)} bps`);
  }
  const keep = BPS_DENOMINATOR - BigInt(toleranceBps);
  return (quote.destinationAmount * keep) / BPS_DENOMINATOR;
}

/** Convenience alias for single rail quote planning. */
export const quoteRoute = planQuote;

/** Convenience alias for all rail quote planning. */
export const quoteAllRails = planQuotes;

export * from "./quotes/types.js";
export * from "./quotes/ladder.js";

/** Additive V2 wrapper. `quote`, planQuote and quoteBatch retain their legacy schema. */
export interface RouteQuoteV2 {
  readonly quote: RouteQuote;
  readonly requestedAmount: bigint;
  readonly fee: bigint;
  readonly netAmount: bigint;
  readonly retainedDust: bigint;
  readonly actualDebit: bigint;
  readonly breakdownKnown: boolean;
}

export function planQuoteV2(request: QuoteRequest, snapshot: RouterSnapshot): RouteQuoteV2 {
  const quote = planQuote(request, snapshot);
  const base = {
    quote,
    requestedAmount: request.amount,
    fee: 0n,
    netAmount: 0n,
    retainedDust: 0n,
    actualDebit: 0n,
    breakdownKnown: false,
  };
  if (
    !snapshot.tokenRegistered ||
    request.amount <= 0n ||
    request.amount > MAX_QUOTE_AMOUNT ||
    snapshot.tokenDecimals > MAX_DECIMALS ||
    request.destinationDecimals > MAX_DECIMALS
  )
    return base;
  const fee = (request.amount * BigInt(snapshot.feeBps)) / BPS_DENOMINATOR;
  const netAmount = floorToRepresentable(
    request.amount - fee,
    snapshot.tokenDecimals,
    request.destinationDecimals,
  );
  return {
    ...base,
    fee,
    netAmount,
    retainedDust: request.amount - fee - netAmount,
    actualDebit: fee + netAmount,
    breakdownKnown: true,
  };
}
