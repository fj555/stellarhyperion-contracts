#!/usr/bin/env node
/**
 * Axelar paid-dispatch authorization-tree snapshot inspector.
 *
 * Inspects nested sender/token/gas-service authorizations for Axelar paid ITS dispatches
 * and detects unexpected privilege expansion (changed spender, changed gas recipient,
 * amount expansion, or extraneous sub-invocations) before signing.
 *
 * Snapshot generation and inspection require no private key or network broadcast.
 *
 * Active deployment is Stellar Testnet outbound ITS/CCTP; EVM and other rail development
 * remain explicitly gated until independently validated.
 */

import { readFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";

/**
 * Exact integer parser for BigInt amounts without floating-point conversion.
 */
export function exactInt(value, field = "amount") {
  if (value === undefined || value === null || String(value).trim() === "") {
    throw new Error(`Missing ${field}; cannot verify amount`);
  }
  if (typeof value === "number" && !Number.isSafeInteger(value)) {
    throw new Error(`${field} must be an exact safe integer or decimal string`);
  }
  const text = String(value).trim();
  if (!/^-?\d+$/.test(text)) {
    throw new Error(`${field} is not a valid integer: ${text}`);
  }
  return BigInt(text);
}

/**
 * Normalizes an address string for comparison (trimmed).
 */
export function normalizeAddress(addr) {
  if (!addr) return "";
  return String(addr).trim();
}

/**
 * Extracts and parses the debits from an Axelar paid dispatch authorization tree.
 *
 * The expected nested call hierarchy is:
 * router.bridge_out_with_gas(sender, request, gasAmount)
 *   -> subInvocation 1: token.transfer(sender, treasury, feeAmount)
 *   -> subInvocation 2: adapter.dispatch_from(caller, sender, token, netAmount, destinationChain, destinationAddress, nonce, gasAmount)
 *        -> its.interchain_transfer(caller, tokenId, destinationChain, destinationAddress, netAmount, metadata, gasToken)
 *             -> token.burn(sender, netAmount) OR token.transfer(sender, manager, netAmount)
 *             -> gasToken.transfer(sender, gasRecipient, gasAmount)
 */
export function parseAuthTreeDebits(authTree, expectedSender) {
  const debits = {
    feeDebit: null,
    principalDebit: null,
    gasDebit: null,
    unexpectedCalls: [],
  };

  if (!authTree || typeof authTree !== "object") {
    return debits;
  }

  const rootSubInvocations = authTree.subInvocations ?? authTree.sub_invocations ?? [];

  for (const sub of rootSubInvocations) {
    const fn = sub.function ?? sub.fn_name ?? sub.fnName;
    const contract = normalizeAddress(sub.contract);
    const args = sub.args ?? {};

    if (fn === "transfer" && (args.from === undefined || normalizeAddress(args.from) === normalizeAddress(expectedSender))) {
      // Direct root transfer is the fee debit to treasury
      debits.feeDebit = {
        contract,
        from: normalizeAddress(args.from ?? expectedSender),
        to: normalizeAddress(args.to),
        amount: exactInt(args.amount, "fee debit amount"),
      };
    } else if (fn === "dispatch_from") {
      // Nested adapter dispatch
      const adapterSubs = sub.subInvocations ?? sub.sub_invocations ?? [];
      for (const adapterSub of adapterSubs) {
        const itsFn = adapterSub.function ?? adapterSub.fn_name ?? adapterSub.fnName;
        if (itsFn === "interchain_transfer") {
          const itsSubs = adapterSub.subInvocations ?? adapterSub.sub_invocations ?? [];
          for (const itsSub of itsSubs) {
            const leafFn = itsSub.function ?? itsSub.fn_name ?? itsSub.fnName;
            const leafContract = normalizeAddress(itsSub.contract);
            const leafArgs = itsSub.args ?? {};

            if (leafFn === "burn") {
              debits.principalDebit = {
                contract: leafContract,
                action: "burn",
                from: normalizeAddress(leafArgs.from ?? expectedSender),
                spender: leafContract, // burn is executed by the token contract
                amount: exactInt(leafArgs.amount, "principal burn amount"),
              };
            } else if (leafFn === "transfer") {
              // Could be principal lock/unlock transfer or gas payment transfer
              const to = normalizeAddress(leafArgs.to);
              const amount = exactInt(leafArgs.amount, "sub transfer amount");
              if (!debits.principalDebit) {
                // If principal not yet assigned and not designated gas
                debits.principalDebit = {
                  contract: leafContract,
                  action: "transfer",
                  from: normalizeAddress(leafArgs.from ?? expectedSender),
                  spender: to,
                  amount,
                };
              } else {
                debits.gasDebit = {
                  contract: leafContract,
                  from: normalizeAddress(leafArgs.from ?? expectedSender),
                  to,
                  amount,
                };
              }
            } else {
              debits.unexpectedCalls.push({
                contract: leafContract,
                function: leafFn,
                args: leafArgs,
              });
            }
          }
        } else {
          debits.unexpectedCalls.push({
            contract: normalizeAddress(adapterSub.contract),
            function: itsFn,
            args: adapterSub.args,
          });
        }
      }
    } else {
      debits.unexpectedCalls.push({
        contract,
        function: fn,
        args,
      });
    }
  }

  return debits;
}

/**
 * Inspect an authorization tree snapshot against the user's expected dispatch intent.
 *
 * Detects:
 * - Changed spender (token manager or unapproved address)
 * - Changed gas recipient (redirected away from ITS / gas service)
 * - Changed fee recipient (redirected away from treasury)
 * - Amount expansion (inflated or mismatched integer debits)
 * - Sender mismatch (debiting unexpected accounts)
 * - Unexpected extra sub-invocations (privilege escalation)
 *
 * @param {object} snapshot The candidate authorization snapshot containing authTree or parsed debits
 * @param {object} expected The baseline expected intent
 * @returns {object} Inspection report
 */
export function inspectAuthSnapshot(snapshot, expected) {
  const privilegeExpansions = [];
  const warnings = [];

  const expectedSender = normalizeAddress(expected.sender ?? snapshot.sender);
  const expectedToken = normalizeAddress(expected.token);
  const expectedSpender = normalizeAddress(expected.expectedSpender ?? expected.tokenManager ?? (expected.isBurn ? expected.token : ""));
  const expectedNetAmount = exactInt(expected.netAmount ?? expected.amount, "expected net amount");
  const expectedFeeRecipient = normalizeAddress(expected.feeRecipient ?? expected.treasury);
  const expectedFeeAmount = exactInt(expected.feeAmount ?? expected.fee ?? 0n, "expected fee amount");
  const expectedGasToken = normalizeAddress(expected.gasToken);
  const expectedGasRecipient = normalizeAddress(expected.gasRecipient ?? expected.its);
  const expectedGasAmount = exactInt(expected.gasAmount ?? expected.gas, "expected gas amount");

  // Determine debits either from structured debits or by parsing the auth tree
  let debits = snapshot.debits;
  if (!debits && snapshot.authTree) {
    debits = parseAuthTreeDebits(snapshot.authTree, expectedSender);
  }

  if (!debits) {
    return {
      safe: false,
      status: "rejected",
      privilegeExpansions: ["MISSING_AUTH_DATA: snapshot contains neither parsed debits nor a valid authTree"],
      warnings: ["Unable to verify authorizations"],
      authorizedDebits: {},
      callTreeVerified: false,
    };
  }

  // 1. Verify Principal Debit & Spender
  const principal = debits.principalDebit;
  if (!principal) {
    privilegeExpansions.push("MISSING_PRINCIPAL_DEBIT: no authorized burn or transfer for principal tokens found in tree");
  } else {
    if (expectedSender && normalizeAddress(principal.from) !== expectedSender) {
      privilegeExpansions.push(
        `UNAPPROVED_SENDER: principal debited from ${principal.from} instead of expected sender ${expectedSender}`,
      );
    }
    if (expectedToken && normalizeAddress(principal.contract) !== expectedToken) {
      privilegeExpansions.push(
        `CHANGED_PRINCIPAL_TOKEN: tokens debited from contract ${principal.contract} instead of expected token ${expectedToken}`,
      );
    }
    if (expectedSpender && normalizeAddress(principal.spender) !== expectedSpender) {
      const msg = `CHANGED_SPENDER: principal tokens are authorized for spender ${principal.spender} instead of expected ${expectedSpender}`;
      privilegeExpansions.push(msg);
      warnings.push(`[ALERT] ${msg}`);
    }
    if (principal.amount !== expectedNetAmount) {
      const msg = `AMOUNT_EXPANSION: authorized principal amount ${principal.amount.toString()} does not match expected integer amount ${expectedNetAmount.toString()}`;
      privilegeExpansions.push(msg);
      warnings.push(`[ALERT] ${msg}`);
    }
  }

  // 2. Verify Fee Debit & Recipient
  const fee = debits.feeDebit;
  if (expectedFeeAmount > 0n) {
    if (!fee) {
      privilegeExpansions.push("MISSING_FEE_DEBIT: expected router fee debit was not found in authorization tree");
    } else {
      if (expectedSender && normalizeAddress(fee.from) !== expectedSender) {
        privilegeExpansions.push(
          `UNAPPROVED_SENDER: fee debited from ${fee.from} instead of expected sender ${expectedSender}`,
        );
      }
      if (expectedFeeRecipient && normalizeAddress(fee.to) !== expectedFeeRecipient) {
        const msg = `CHANGED_FEE_RECIPIENT: fee is authorized for recipient ${fee.to} instead of expected treasury ${expectedFeeRecipient}`;
        privilegeExpansions.push(msg);
        warnings.push(`[ALERT] ${msg}`);
      }
      if (fee.amount !== expectedFeeAmount) {
        const msg = `AMOUNT_EXPANSION: authorized fee amount ${fee.amount.toString()} does not match expected integer fee ${expectedFeeAmount.toString()}`;
        privilegeExpansions.push(msg);
        warnings.push(`[ALERT] ${msg}`);
      }
    }
  }

  // 3. Verify Gas Debit & Recipient
  const gas = debits.gasDebit;
  if (!gas) {
    privilegeExpansions.push("MISSING_GAS_DEBIT: expected native gas service payment was not found in authorization tree");
  } else {
    if (expectedSender && normalizeAddress(gas.from) !== expectedSender) {
      privilegeExpansions.push(
        `UNAPPROVED_SENDER: gas debited from ${gas.from} instead of expected sender ${expectedSender}`,
      );
    }
    if (expectedGasToken && normalizeAddress(gas.contract) !== expectedGasToken) {
      privilegeExpansions.push(
        `CHANGED_GAS_TOKEN: gas debited using contract ${gas.contract} instead of expected gas token ${expectedGasToken}`,
      );
    }
    if (expectedGasRecipient && normalizeAddress(gas.to) !== expectedGasRecipient) {
      const msg = `CHANGED_GAS_RECIPIENT: gas tokens are authorized for recipient ${gas.to} instead of expected ${expectedGasRecipient}`;
      privilegeExpansions.push(msg);
      warnings.push(`[ALERT] ${msg}`);
    }
    if (gas.amount !== expectedGasAmount) {
      const msg = `AMOUNT_EXPANSION: authorized gas amount ${gas.amount.toString()} does not match expected integer gas amount ${expectedGasAmount.toString()}`;
      privilegeExpansions.push(msg);
      warnings.push(`[ALERT] ${msg}`);
    }
  }

  // 4. Verify No Extraneous / Unexpected Sub-Invocations
  if (debits.unexpectedCalls && debits.unexpectedCalls.length > 0) {
    for (const call of debits.unexpectedCalls) {
      const msg = `UNEXPECTED_SUB_INVOCATION: authorization tree contains unapproved call to ${call.contract}.${call.function}`;
      privilegeExpansions.push(msg);
      warnings.push(`[CRITICAL] ${msg}`);
    }
  }

  const safe = privilegeExpansions.length === 0;

  return {
    safe,
    status: safe ? "approved" : "rejected",
    privilegeExpansions,
    warnings,
    authorizedDebits: {
      principal: principal ? {
        contract: principal.contract,
        spender: principal.spender,
        action: principal.action,
        amount: principal.amount.toString(),
      } : null,
      fee: fee ? {
        contract: fee.contract,
        recipient: fee.to,
        amount: fee.amount.toString(),
      } : null,
      gas: gas ? {
        contract: gas.contract,
        recipient: gas.to,
        amount: gas.amount.toString(),
      } : null,
    },
    callTreeVerified: safe,
  };
}

/**
 * Generate a canonical expected authorization snapshot deterministically.
 * Requires no private key or network broadcast.
 */
export function generateAuthSnapshot({
  sender,
  token,
  netAmount,
  expectedSpender,
  isBurn = true,
  feeRecipient,
  feeAmount = "0",
  gasToken,
  gasRecipient,
  gasAmount,
  routerAddress = "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC",
  adapterAddress = "CBBD7PQXN7P55U25D2BWWX7AUKC7E3AOP7XGQLB7IYY3Z3QWVQO32AAA",
  itsAddress = gasRecipient,
  destinationChain = "ethereum",
  destinationAddress = "0x1111111111111111111111111111111111111111",
  nonce = "1",
}) {
  const normSender = normalizeAddress(sender);
  const normToken = normalizeAddress(token);
  const normSpender = normalizeAddress(expectedSpender ?? (isBurn ? normToken : ""));
  const normNetAmt = exactInt(netAmount, "net amount").toString();
  const normFeeRecipient = normalizeAddress(feeRecipient);
  const normFeeAmt = exactInt(feeAmount, "fee amount").toString();
  const normGasToken = normalizeAddress(gasToken);
  const normGasRecipient = normalizeAddress(gasRecipient);
  const normGasAmt = exactInt(gasAmount, "gas amount").toString();

  const leafInvocations = [];
  if (isBurn) {
    leafInvocations.push({
      contract: normToken,
      function: "burn",
      args: {
        from: normSender,
        amount: normNetAmt,
      },
      subInvocations: [],
    });
  } else {
    leafInvocations.push({
      contract: normToken,
      function: "transfer",
      args: {
        from: normSender,
        to: normSpender,
        amount: normNetAmt,
      },
      subInvocations: [],
    });
  }

  leafInvocations.push({
    contract: normGasToken,
    function: "transfer",
    args: {
      from: normSender,
      to: normGasRecipient,
      amount: normGasAmt,
    },
    subInvocations: [],
  });

  const rootSubInvocations = [];
  if (normFeeAmt !== "0") {
    rootSubInvocations.push({
      contract: normToken,
      function: "transfer",
      args: {
        from: normSender,
        to: normFeeRecipient,
        amount: normFeeAmt,
      },
      subInvocations: [],
    });
  }

  rootSubInvocations.push({
    contract: adapterAddress,
    function: "dispatch_from",
    args: {
      caller: routerAddress,
      sender: normSender,
      token: normToken,
      amount: normNetAmt,
      destinationChain,
      destination: destinationAddress,
      nonce,
      gasAmount: normGasAmt,
    },
    subInvocations: [
      {
        contract: itsAddress,
        function: "interchain_transfer",
        args: {
          caller: normSender,
          destinationChain,
          amount: normNetAmt,
        },
        subInvocations: leafInvocations,
      },
    ],
  });

  return {
    sender: normSender,
    expectedIntent: {
      sender: normSender,
      token: normToken,
      netAmount: normNetAmt,
      expectedSpender: normSpender,
      isBurn,
      feeRecipient: normFeeRecipient,
      feeAmount: normFeeAmt,
      gasToken: normGasToken,
      gasRecipient: normGasRecipient,
      gasAmount: normGasAmt,
    },
    authTree: {
      contract: routerAddress,
      function: "bridge_out_with_gas",
      args: {
        sender: normSender,
        token: normToken,
        amount: (BigInt(normNetAmt) + BigInt(normFeeAmt)).toString(),
        destinationChain,
        destinationAddress,
        gasAmount: normGasAmt,
      },
      subInvocations: rootSubInvocations,
    },
  };
}

function printUsage() {
  console.log(`
Axelar Paid-Dispatch Authorization Tree Inspector

Usage:
  node script/stellar/inspect-axelar-auth.mjs [options]

Options:
  --snapshot <path>      Path to JSON file containing candidate authorization tree snapshot
  --fixture <dir>        Path to fixture directory containing snapshot.json
  --candidate <path>     Path to candidate snapshot JSON
  --expected <path>      Path to baseline expected intent JSON
  --strict               Exit with code 1 if unexpected privilege expansion is detected
  --pretty               Format JSON output (default: true)
  --generate             Generate canonical expected snapshot deterministically
  -h, --help             Show this help message

Generate Flags (with --generate):
  --sender <addr>        Sender Stellar account strkey
  --token <addr>         Principal token contract address
  --amount <amt>         Net transfer amount (integer)
  --manager <addr>       Expected token manager contract address (for lock/unlock)
  --burn                 Principal token takes by burning (default: true)
  --fee-recipient <addr> Treasury address receiving fee
  --fee <amt>            Fee amount (default: 0)
  --gas-token <addr>     Native gas token address
  --gas-recipient <addr> Gas recipient (ITS or gas service contract address)
  --gas <amt>            Gas payment amount (integer)
`);
}

function parseCliArgs(argv) {
  const args = {
    snapshotPath: null,
    fixtureDir: null,
    candidatePath: null,
    expectedPath: null,
    strict: false,
    pretty: true,
    generate: false,
    generateParams: {},
  };

  for (let i = 2; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") {
      printUsage();
      process.exit(0);
    } else if (arg === "--snapshot" && i + 1 < argv.length) {
      args.snapshotPath = argv[++i];
    } else if (arg === "--fixture" && i + 1 < argv.length) {
      args.fixtureDir = argv[++i];
    } else if (arg === "--candidate" && i + 1 < argv.length) {
      args.candidatePath = argv[++i];
    } else if (arg === "--expected" && i + 1 < argv.length) {
      args.expectedPath = argv[++i];
    } else if (arg === "--strict") {
      args.strict = true;
    } else if (arg === "--pretty") {
      args.pretty = true;
    } else if (arg === "--generate") {
      args.generate = true;
    } else if (arg === "--sender" && i + 1 < argv.length) {
      args.generateParams.sender = argv[++i];
    } else if (arg === "--token" && i + 1 < argv.length) {
      args.generateParams.token = argv[++i];
    } else if (arg === "--amount" && i + 1 < argv.length) {
      args.generateParams.netAmount = argv[++i];
    } else if (arg === "--manager" && i + 1 < argv.length) {
      args.generateParams.expectedSpender = argv[++i];
      args.generateParams.isBurn = false;
    } else if (arg === "--burn") {
      args.generateParams.isBurn = true;
    } else if (arg === "--fee-recipient" && i + 1 < argv.length) {
      args.generateParams.feeRecipient = argv[++i];
    } else if (arg === "--fee" && i + 1 < argv.length) {
      args.generateParams.feeAmount = argv[++i];
    } else if (arg === "--gas-token" && i + 1 < argv.length) {
      args.generateParams.gasToken = argv[++i];
    } else if (arg === "--gas-recipient" && i + 1 < argv.length) {
      args.generateParams.gasRecipient = argv[++i];
    } else if (arg === "--gas" && i + 1 < argv.length) {
      args.generateParams.gasAmount = argv[++i];
    }
  }
  return args;
}

export function runCli(argv = process.argv) {
  const args = parseCliArgs(argv);

  if (args.generate) {
    const generated = generateAuthSnapshot(args.generateParams);
    console.log(JSON.stringify(generated, null, 2));
    return generated;
  }

  let snapshot = null;
  let expected = null;

  if (args.fixtureDir) {
    const fixturePath = resolve(args.fixtureDir);
    const snapshotFile = join(fixturePath, "snapshot.json");
    snapshot = JSON.parse(readFileSync(snapshotFile, "utf8"));
    expected = snapshot.expectedIntent ?? snapshot.expected;
  } else if (args.snapshotPath) {
    snapshot = JSON.parse(readFileSync(resolve(args.snapshotPath), "utf8"));
    expected = snapshot.expectedIntent ?? snapshot.expected;
  } else if (args.candidatePath) {
    snapshot = JSON.parse(readFileSync(resolve(args.candidatePath), "utf8"));
    if (args.expectedPath) {
      expected = JSON.parse(readFileSync(resolve(args.expectedPath), "utf8"));
    } else {
      expected = snapshot.expectedIntent ?? snapshot.expected;
    }
  } else if (!process.stdin.isTTY) {
    const input = readFileSync(0, "utf8");
    snapshot = JSON.parse(input);
    expected = snapshot.expectedIntent ?? snapshot.expected;
  } else {
    printUsage();
    process.exit(1);
  }

  if (!expected) {
    expected = snapshot.expectedIntent ?? snapshot;
  }

  const report = inspectAuthSnapshot(snapshot, expected);

  // Highlight warnings to stderr for immediate pre-signing visibility
  if (report.warnings.length > 0) {
    console.error("\n=======================================================");
    console.error("  PRE-SIGNING AUTHORIZATION EXPANSION WARNINGS DETECTED ");
    console.error("=======================================================");
    for (const warn of report.warnings) {
      console.error(warn);
    }
    console.error("=======================================================\n");
  }

  const output = args.pretty ? JSON.stringify(report, null, 2) : JSON.stringify(report);
  console.log(output);

  if (args.strict && !report.safe) {
    process.exit(1);
  }

  return report;
}

const isMain = process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url;
if (isMain) {
  runCli(process.argv);
}
