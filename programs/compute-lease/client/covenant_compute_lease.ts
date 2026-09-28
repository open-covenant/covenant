/**
 * Program IDL in camelCase format in order to be used in JS/TS.
 *
 * Note that this is only a type helper and is not the actual IDL. The original
 * IDL can be found at `target/idl/covenant_compute_lease.json`.
 */
export type CovenantComputeLease = {
  "address": "CLSeVNrRi4TpXsXAkAuLh58kGCCAd1w1bj2CcEhTEESd",
  "metadata": {
    "name": "covenantComputeLease",
    "version": "0.1.0",
    "spec": "0.1.0",
    "description": "Per-second GPU lease metering on a MagicBlock Ephemeral Rollup, escrowed and settled in USDC on Solana."
  },
  "instructions": [
    {
      "name": "claimOperatorShare",
      "docs": [
        "Pays the operator's share on its own.",
        "",
        "The two payouts are separable because one blocked destination must",
        "not hold the other side's money. A settlement mint with a live",
        "freeze authority, or a party that simply closed its token account,",
        "would otherwise revert every settlement and strand the whole",
        "escrow rather than just that party's share."
      ],
      "discriminator": [
        48,
        94,
        26,
        230,
        17,
        207,
        37,
        172
      ],
      "accounts": [
        {
          "name": "payer",
          "writable": true,
          "signer": true
        },
        {
          "name": "terms",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  97,
                  115,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "terms.renter",
                "account": "leaseTerms"
              },
              {
                "kind": "account",
                "path": "terms.job_id",
                "account": "leaseTerms"
              }
            ]
          }
        },
        {
          "name": "meter",
          "docs": [
            "program owns it."
          ],
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  101,
                  116,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "terms"
              }
            ]
          }
        },
        {
          "name": "mint",
          "relations": [
            "terms"
          ]
        },
        {
          "name": "vault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "terms"
              }
            ]
          }
        },
        {
          "name": "operatorTokens",
          "writable": true
        },
        {
          "name": "tokenProgram"
        }
      ],
      "args": []
    },
    {
      "name": "claimRenterRefund",
      "docs": [
        "Returns the renter's remainder on its own. The operator's share",
        "stays reserved in the vault until it is claimed, so calling this",
        "first cannot take the money out from under them."
      ],
      "discriminator": [
        127,
        108,
        137,
        251,
        222,
        91,
        39,
        29
      ],
      "accounts": [
        {
          "name": "payer",
          "writable": true,
          "signer": true
        },
        {
          "name": "terms",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  97,
                  115,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "terms.renter",
                "account": "leaseTerms"
              },
              {
                "kind": "account",
                "path": "terms.job_id",
                "account": "leaseTerms"
              }
            ]
          }
        },
        {
          "name": "meter",
          "docs": [
            "program owns it."
          ],
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  101,
                  116,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "terms"
              }
            ]
          }
        },
        {
          "name": "mint",
          "relations": [
            "terms"
          ]
        },
        {
          "name": "vault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "terms"
              }
            ]
          }
        },
        {
          "name": "renterTokens",
          "writable": true
        },
        {
          "name": "tokenProgram"
        }
      ],
      "args": []
    },
    {
      "name": "delegateLease",
      "docs": [
        "Hands the meter to the rollup validator the renter pinned at open",
        "so ticks can run there.",
        "",
        "Coordinator-signed and validator-checked. Delegation is the act of",
        "giving an account to a third party that then writes its state",
        "back, so an open door here is an unauthenticated transfer of the",
        "session's meter to a host of the caller's choosing — including one",
        "that does not exist, which would leave the meter unreachable for",
        "the rest of the lease."
      ],
      "discriminator": [
        208,
        75,
        250,
        115,
        2,
        133,
        112,
        51
      ],
      "accounts": [
        {
          "name": "payer",
          "writable": true,
          "signer": true
        },
        {
          "name": "terms",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  97,
                  115,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "terms.renter",
                "account": "leaseTerms"
              },
              {
                "kind": "account",
                "path": "terms.job_id",
                "account": "leaseTerms"
              }
            ]
          }
        },
        {
          "name": "coordinator",
          "signer": true,
          "relations": [
            "terms"
          ]
        },
        {
          "name": "erValidator",
          "relations": [
            "terms"
          ]
        },
        {
          "name": "bufferMeter",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  117,
                  102,
                  102,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "meter"
              }
            ],
            "program": {
              "kind": "const",
              "value": [
                168,
                107,
                151,
                10,
                83,
                78,
                247,
                223,
                35,
                74,
                61,
                160,
                35,
                22,
                216,
                186,
                7,
                251,
                81,
                236,
                117,
                248,
                2,
                193,
                123,
                69,
                181,
                184,
                98,
                90,
                247,
                10
              ]
            }
          }
        },
        {
          "name": "delegationRecordMeter",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  100,
                  101,
                  108,
                  101,
                  103,
                  97,
                  116,
                  105,
                  111,
                  110
                ]
              },
              {
                "kind": "account",
                "path": "meter"
              }
            ],
            "program": {
              "kind": "account",
              "path": "delegationProgram"
            }
          }
        },
        {
          "name": "delegationMetadataMeter",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  100,
                  101,
                  108,
                  101,
                  103,
                  97,
                  116,
                  105,
                  111,
                  110,
                  45,
                  109,
                  101,
                  116,
                  97,
                  100,
                  97,
                  116,
                  97
                ]
              },
              {
                "kind": "account",
                "path": "meter"
              }
            ],
            "program": {
              "kind": "account",
              "path": "delegationProgram"
            }
          }
        },
        {
          "name": "meter",
          "docs": [
            "delegation zeroes the account and reassigns its owner, which a",
            "typed account would try to write back over."
          ],
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  101,
                  116,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "terms"
              }
            ]
          }
        },
        {
          "name": "ownerProgram",
          "address": "CLSeVNrRi4TpXsXAkAuLh58kGCCAd1w1bj2CcEhTEESd"
        },
        {
          "name": "delegationProgram",
          "address": "DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": []
    },
    {
      "name": "openLease",
      "docs": [
        "Opens a lease and escrows the whole window in one step.",
        "",
        "The renter signs, and in signing names the coordinator that may",
        "meter them and the rollup validator that may host that meter.",
        "Both are recorded, so a buyer reading the chain can see who was",
        "authorised before a single second was billed, and neither can be",
        "swapped afterwards.",
        "",
        "The renter is part of the lease address. Without that, anyone who",
        "learned a job id — the assigned operator learns it at dispatch —",
        "could occupy the address first with terms of their own and the",
        "real open would fail for good, quietly downgrading the session to",
        "an off-chain meter."
      ],
      "discriminator": [
        187,
        79,
        139,
        164,
        14,
        110,
        255,
        127
      ],
      "accounts": [
        {
          "name": "renter",
          "writable": true,
          "signer": true
        },
        {
          "name": "operator"
        },
        {
          "name": "coordinator",
          "docs": [
            "renter is agreeing here to who observes them, so it is named at",
            "open and fixed for the life of the lease."
          ]
        },
        {
          "name": "erValidator",
          "docs": [
            "delegated to."
          ]
        },
        {
          "name": "terms",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  97,
                  115,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "renter"
              },
              {
                "kind": "arg",
                "path": "jobId"
              }
            ]
          }
        },
        {
          "name": "meter",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  101,
                  116,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "terms"
              }
            ]
          }
        },
        {
          "name": "mint"
        },
        {
          "name": "vault",
          "docs": [
            "A PDA rather than an associated token account: only this program",
            "can create an account at this address, so the vault cannot be",
            "created ahead of the open to make the open fail."
          ],
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "terms"
              }
            ]
          }
        },
        {
          "name": "renterTokens",
          "writable": true
        },
        {
          "name": "tokenProgram"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "jobId",
          "type": {
            "array": [
              "u8",
              16
            ]
          }
        },
        {
          "name": "rateMicroUsdcPerSec",
          "type": "u64"
        },
        {
          "name": "maxDurationSecs",
          "type": "u64"
        }
      ]
    },
    {
      "name": "processUndelegation",
      "discriminator": [
        196,
        28,
        41,
        206,
        48,
        37,
        51,
        167
      ],
      "accounts": [
        {
          "name": "baseAccount",
          "writable": true
        },
        {
          "name": "buffer"
        },
        {
          "name": "payer",
          "writable": true
        },
        {
          "name": "systemProgram"
        }
      ],
      "args": [
        {
          "name": "accountSeeds",
          "type": {
            "vec": "bytes"
          }
        }
      ]
    },
    {
      "name": "settleLease",
      "docs": [
        "Pays the operator what the meter says and returns the rest to the",
        "renter, in one transaction.",
        "",
        "Permissionless, but only once the lease is actually over: the",
        "meter came back from the rollup, the lease was voided, or the",
        "window the renter escrowed has elapsed. An unconditional door here",
        "would let a renter settle at zero in the slot after the open,",
        "before the coordinator's delegate lands, and keep a full session",
        "of compute for nothing."
      ],
      "discriminator": [
        80,
        14,
        40,
        219,
        201,
        133,
        236,
        90
      ],
      "accounts": [
        {
          "name": "payer",
          "writable": true,
          "signer": true
        },
        {
          "name": "terms",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  97,
                  115,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "terms.renter",
                "account": "leaseTerms"
              },
              {
                "kind": "account",
                "path": "terms.job_id",
                "account": "leaseTerms"
              }
            ]
          }
        },
        {
          "name": "meter",
          "docs": [
            "that is still delegated is owned by the delegation program;",
            "`settlement_figures` reads it only when this program owns it."
          ],
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  101,
                  116,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "terms"
              }
            ]
          }
        },
        {
          "name": "renter",
          "docs": [
            "vault is closed. They paid it at open."
          ],
          "writable": true
        },
        {
          "name": "mint",
          "relations": [
            "terms"
          ]
        },
        {
          "name": "vault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "terms"
              }
            ]
          }
        },
        {
          "name": "operatorTokens",
          "writable": true
        },
        {
          "name": "renterTokens",
          "writable": true
        },
        {
          "name": "tokenProgram"
        }
      ],
      "args": []
    },
    {
      "name": "tick",
      "docs": [
        "Records the seconds served so far and folds the tick's receipt",
        "hash into the provenance chain. Runs in the ER, so a per-second",
        "meter costs nothing to keep.",
        "",
        "`metered_ms` is cumulative rather than a delta: a lost or",
        "duplicated tick then costs nothing, because settlement is always",
        "recomputed from the total elapsed. A tick that would go backwards",
        "is refused — the meter only ever moves forward.",
        "",
        "No money is computed here. The rollup host can rewrite anything in",
        "this account when it commits, so the rate and the escrow are kept",
        "on L1 and the charge is derived there at settlement."
      ],
      "discriminator": [
        92,
        79,
        44,
        8,
        101,
        80,
        63,
        15
      ],
      "accounts": [
        {
          "name": "meter",
          "writable": true
        },
        {
          "name": "coordinator",
          "signer": true,
          "relations": [
            "meter"
          ]
        }
      ],
      "args": [
        {
          "name": "meteredMs",
          "type": "u64"
        },
        {
          "name": "receiptHash",
          "type": {
            "array": [
              "u8",
              32
            ]
          }
        }
      ]
    },
    {
      "name": "undelegateLease",
      "docs": [
        "Closes the meter and commits it back to L1, which is what makes",
        "settlement possible.",
        "",
        "The `concluded` flag rides the same commit. Without it the meter",
        "is writable again the moment it lands on L1, and the coordinator",
        "could raise the elapsed after the renter has already reconciled",
        "the committed figure and before anyone settles it.",
        "",
        "Coordinator-signed, for the same reason ticking is: ending the",
        "meter early is worth exactly as much as under-reporting it, so a",
        "renter must not be able to cut a live session's meter two seconds",
        "in and settle for one tick of a window they are still using."
      ],
      "discriminator": [
        161,
        95,
        240,
        230,
        35,
        21,
        136,
        242
      ],
      "accounts": [
        {
          "name": "payer",
          "writable": true,
          "signer": true
        },
        {
          "name": "meter",
          "writable": true
        },
        {
          "name": "coordinator",
          "signer": true,
          "relations": [
            "meter"
          ]
        },
        {
          "name": "magicProgram",
          "address": "Magic11111111111111111111111111111111111111"
        },
        {
          "name": "magicContext",
          "writable": true,
          "address": "MagicContext1111111111111111111111111111111"
        }
      ],
      "args": []
    },
    {
      "name": "voidLease",
      "docs": [
        "Cancels the charge and opens settlement immediately: the renter",
        "gets the whole vault back and the operator gets nothing.",
        "",
        "The marketplace has terminal paths — a deadline that expired, a",
        "receipt that came back failed, an expired session swept — where",
        "the buyer is refunded in full off-chain. The meter is monotonic",
        "and cannot be wound back, so without this a lease that took ticks",
        "on one of those paths would still pay the operator on-chain for",
        "work the marketplace already refused to bill, and the escrow would",
        "sit funded until someone unwound it by hand.",
        "",
        "Coordinator-signed, which grants no power it did not already have:",
        "the party that decides what the meter says can already decide it",
        "says zero."
      ],
      "discriminator": [
        62,
        166,
        226,
        18,
        70,
        55,
        61,
        3
      ],
      "accounts": [
        {
          "name": "terms",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  97,
                  115,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "terms.renter",
                "account": "leaseTerms"
              },
              {
                "kind": "account",
                "path": "terms.job_id",
                "account": "leaseTerms"
              }
            ]
          }
        },
        {
          "name": "coordinator",
          "signer": true,
          "relations": [
            "terms"
          ]
        }
      ],
      "args": []
    }
  ],
  "accounts": [
    {
      "name": "leaseMeter",
      "discriminator": [
        254,
        152,
        135,
        84,
        146,
        89,
        109,
        232
      ]
    },
    {
      "name": "leaseTerms",
      "discriminator": [
        120,
        89,
        222,
        140,
        162,
        246,
        170,
        179
      ]
    }
  ],
  "events": [
    {
      "name": "leaseOpened",
      "discriminator": [
        55,
        230,
        185,
        195,
        231,
        104,
        170,
        89
      ]
    },
    {
      "name": "leaseOperatorPaid",
      "discriminator": [
        240,
        34,
        101,
        137,
        240,
        203,
        50,
        155
      ]
    },
    {
      "name": "leaseRenterRefunded",
      "discriminator": [
        101,
        10,
        76,
        216,
        184,
        182,
        121,
        251
      ]
    },
    {
      "name": "leaseSettled",
      "discriminator": [
        229,
        233,
        206,
        105,
        201,
        233,
        216,
        110
      ]
    },
    {
      "name": "leaseTicked",
      "discriminator": [
        128,
        134,
        146,
        254,
        64,
        38,
        131,
        227
      ]
    },
    {
      "name": "leaseVoided",
      "discriminator": [
        249,
        33,
        37,
        207,
        180,
        224,
        216,
        153
      ]
    }
  ],
  "errors": [
    {
      "code": 6000,
      "name": "zeroRate",
      "msg": "a lease rate must be greater than zero"
    },
    {
      "code": 6001,
      "name": "badDuration",
      "msg": "a lease window must be between one second and a day"
    },
    {
      "code": 6002,
      "name": "overflow",
      "msg": "the lease window overflows"
    },
    {
      "code": 6003,
      "name": "emptyDeposit",
      "msg": "the escrow deposit arrived empty"
    },
    {
      "code": 6004,
      "name": "alreadySettled",
      "msg": "this lease is already settled"
    },
    {
      "code": 6005,
      "name": "alreadyPaid",
      "msg": "this share of the lease is already paid"
    },
    {
      "code": 6006,
      "name": "alreadyVoided",
      "msg": "this lease is already voided"
    },
    {
      "code": 6007,
      "name": "leaseVoided",
      "msg": "this lease is voided"
    },
    {
      "code": 6008,
      "name": "meterWentBackwards",
      "msg": "a meter may only move forward"
    },
    {
      "code": 6009,
      "name": "meterClosed",
      "msg": "this meter is closed"
    },
    {
      "code": 6010,
      "name": "leaseStillRunning",
      "msg": "this lease cannot be settled until its meter is concluded or its window has elapsed"
    }
  ],
  "types": [
    {
      "name": "leaseMeter",
      "docs": [
        "The part that runs in the rollup. Elapsed time and a hash chain, and",
        "nothing a host could rewrite into a payout."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "terms",
            "type": "pubkey"
          },
          {
            "name": "coordinator",
            "docs": [
              "Carried alongside the terms so a tick can be authenticated inside",
              "the rollup, where the terms account is not present."
            ],
            "type": "pubkey"
          },
          {
            "name": "jobId",
            "type": {
              "array": [
                "u8",
                16
              ]
            }
          },
          {
            "name": "meteredMs",
            "docs": [
              "Cumulative session time the coordinator has observed."
            ],
            "type": "u64"
          },
          {
            "name": "provenanceRoot",
            "docs": [
              "`root = sha256(root || receipt_hash)` over every tick, genesis 32",
              "zero bytes. Committed to L1 with the elapsed, so the settled",
              "amount arrives with a replayable record of how it was reached."
            ],
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "concluded",
            "docs": [
              "Set by the undelegate that commits this meter to L1. A concluded",
              "meter takes no further ticks, so the figure a renter reconciles at",
              "commit time is the figure that settles."
            ],
            "type": "bool"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "leaseOpened",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "jobId",
            "type": {
              "array": [
                "u8",
                16
              ]
            }
          },
          {
            "name": "renter",
            "type": "pubkey"
          },
          {
            "name": "operator",
            "type": "pubkey"
          },
          {
            "name": "coordinator",
            "type": "pubkey"
          },
          {
            "name": "erValidator",
            "type": "pubkey"
          },
          {
            "name": "mint",
            "type": "pubkey"
          },
          {
            "name": "rateMicroUsdcPerSec",
            "type": "u64"
          },
          {
            "name": "maxDurationSecs",
            "type": "u64"
          },
          {
            "name": "fundedMicroUsdc",
            "type": "u64"
          },
          {
            "name": "openedAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "leaseOperatorPaid",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "jobId",
            "type": {
              "array": [
                "u8",
                16
              ]
            }
          },
          {
            "name": "operator",
            "type": "pubkey"
          },
          {
            "name": "meteredMs",
            "type": "u64"
          },
          {
            "name": "amountMicroUsdc",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "leaseRenterRefunded",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "jobId",
            "type": {
              "array": [
                "u8",
                16
              ]
            }
          },
          {
            "name": "renter",
            "type": "pubkey"
          },
          {
            "name": "amountMicroUsdc",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "leaseSettled",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "jobId",
            "type": {
              "array": [
                "u8",
                16
              ]
            }
          },
          {
            "name": "meteredMs",
            "type": "u64"
          },
          {
            "name": "chargedMicroUsdc",
            "type": "u64"
          },
          {
            "name": "refundedMicroUsdc",
            "type": "u64"
          },
          {
            "name": "provenanceRoot",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "voided",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "leaseTerms",
      "docs": [
        "Everything that decides money. Never delegated, so the rollup host",
        "that writes the meter cannot name itself the operator, raise the rate,",
        "or move the escrow."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "jobId",
            "docs": [
              "The coordinator's job id, as raw uuid bytes — the same identifier",
              "the signed work receipt carries, so a reader can line the two up."
            ],
            "type": {
              "array": [
                "u8",
                16
              ]
            }
          },
          {
            "name": "renter",
            "type": "pubkey"
          },
          {
            "name": "operator",
            "type": "pubkey"
          },
          {
            "name": "coordinator",
            "docs": [
              "The only key that may meter, delegate, conclude or void this",
              "lease. Named by the renter at open."
            ],
            "type": "pubkey"
          },
          {
            "name": "erValidator",
            "docs": [
              "The only rollup identity the meter may be delegated to."
            ],
            "type": "pubkey"
          },
          {
            "name": "mint",
            "type": "pubkey"
          },
          {
            "name": "rateMicroUsdcPerSec",
            "type": "u64"
          },
          {
            "name": "maxDurationSecs",
            "type": "u64"
          },
          {
            "name": "fundedMicroUsdc",
            "docs": [
              "What the vault actually received at open, which is not",
              "necessarily what was asked for."
            ],
            "type": "u64"
          },
          {
            "name": "openedAt",
            "docs": [
              "Unix seconds at open. The window ends `max_duration_secs` later,",
              "and that is what makes permissionless settlement safe."
            ],
            "type": "i64"
          },
          {
            "name": "paidOperator",
            "type": "bool"
          },
          {
            "name": "paidRenter",
            "type": "bool"
          },
          {
            "name": "voided",
            "docs": [
              "Set when the marketplace refused to bill the session at all: the",
              "charge becomes zero and the whole vault goes back to the renter."
            ],
            "type": "bool"
          },
          {
            "name": "delegated",
            "docs": [
              "Set the first time the meter is handed to the rollup. A meter this",
              "program owns again after that has been through the rollup and come",
              "back, which is the second, independent signal that the lease is",
              "over."
            ],
            "type": "bool"
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "meterBump",
            "type": "u8"
          },
          {
            "name": "vaultBump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "leaseTicked",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "jobId",
            "type": {
              "array": [
                "u8",
                16
              ]
            }
          },
          {
            "name": "meteredMs",
            "type": "u64"
          },
          {
            "name": "receiptHash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "provenanceRoot",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          }
        ]
      }
    },
    {
      "name": "leaseVoided",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "jobId",
            "type": {
              "array": [
                "u8",
                16
              ]
            }
          },
          {
            "name": "coordinator",
            "type": "pubkey"
          }
        ]
      }
    }
  ]
};
