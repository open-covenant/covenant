// Generated from the on-chain program IDL — do not edit by hand.
// Anchor IDL for the covenant settlement program (cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y).
export const settlementIdl = {
  "address": "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y",
  "metadata": {
    "name": "settlement",
    "version": "0.1.0",
    "spec": "0.1.0",
    "description": "Covenant Solana protocol program for external COVNT staking, credits, escrow, and receipt anchors."
  },
  "instructions": [
    {
      "name": "anchor_receipt_batch",
      "discriminator": [
        90,
        207,
        91,
        121,
        57,
        61,
        176,
        129
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "batch",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  114,
                  101,
                  99,
                  101,
                  105,
                  112,
                  116,
                  95,
                  98,
                  97,
                  116,
                  99,
                  104
                ]
              },
              {
                "kind": "arg",
                "path": "args.batch_id"
              }
            ]
          }
        },
        {
          "name": "authority",
          "writable": true,
          "signer": true,
          "relations": [
            "config"
          ]
        },
        {
          "name": "system_program",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "AnchorReceiptBatchArgs"
            }
          }
        }
      ]
    },
    {
      "name": "burn_covnt",
      "discriminator": [
        108,
        94,
        140,
        107,
        104,
        29,
        96,
        42
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "covnt_mint",
          "writable": true
        },
        {
          "name": "owner_covnt",
          "writable": true
        },
        {
          "name": "token_program"
        }
      ],
      "args": [
        {
          "name": "amount",
          "type": "u64"
        },
        {
          "name": "reason_hash",
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
      "name": "buy_credits",
      "discriminator": [
        14,
        173,
        58,
        38,
        248,
        235,
        115,
        102
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "credits",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  114,
                  101,
                  100,
                  105,
                  116,
                  115
                ]
              },
              {
                "kind": "account",
                "path": "owner"
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true,
          "relations": [
            "credits"
          ]
        },
        {
          "name": "owner_covnt",
          "writable": true
        },
        {
          "name": "treasury",
          "writable": true,
          "relations": [
            "config"
          ]
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "token_program"
        }
      ],
      "args": [
        {
          "name": "amount_covnt",
          "type": "u64"
        }
      ]
    },
    {
      "name": "claim_task",
      "docs": [
        "Provider recourse: once the review window after submission has elapsed",
        "with no release and no arbiter refund, the provider claims the escrow",
        "itself. This is what stops a silent client from stranding delivered",
        "work, the exact gap that kept task escrow disabled before."
      ],
      "discriminator": [
        49,
        222,
        219,
        238,
        155,
        68,
        221,
        136
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "task",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  116,
                  97,
                  115,
                  107
                ]
              },
              {
                "kind": "account",
                "path": "task.task_id",
                "account": "Task"
              }
            ]
          }
        },
        {
          "name": "provider",
          "signer": true
        },
        {
          "name": "escrow_vault",
          "writable": true
        },
        {
          "name": "provider_covnt",
          "writable": true
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "token_program"
        }
      ],
      "args": []
    },
    {
      "name": "close_position",
      "docs": [
        "Owner-signed reclaim of a spent position account (rent returned to",
        "the owner). A position is spent once it is fully slashed",
        "(`amount == 0`, `active == false`); the normal exit path closes the",
        "position inside `unstake`. This exists so a fully-slashed owner can",
        "reclaim rent and re-stake against the same agent."
      ],
      "discriminator": [
        123,
        134,
        81,
        0,
        49,
        68,
        98,
        98
      ],
      "accounts": [
        {
          "name": "position",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  116,
                  97,
                  107,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "position.agent_key",
                "account": "StakePosition"
              },
              {
                "kind": "account",
                "path": "position.owner",
                "account": "StakePosition"
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        }
      ],
      "args": []
    },
    {
      "name": "consume_credits",
      "discriminator": [
        44,
        77,
        95,
        16,
        223,
        42,
        48,
        97
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "credits",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  114,
                  101,
                  100,
                  105,
                  116,
                  115
                ]
              },
              {
                "kind": "account",
                "path": "owner"
              }
            ]
          }
        },
        {
          "name": "owner",
          "signer": true,
          "relations": [
            "credits"
          ]
        }
      ],
      "args": [
        {
          "name": "amount",
          "type": "u64"
        },
        {
          "name": "receipt_hash",
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
      "name": "create_task",
      "discriminator": [
        194,
        80,
        6,
        180,
        232,
        127,
        48,
        171
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "agent",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  97,
                  103,
                  101,
                  110,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "agent.agent_key",
                "account": "Agent"
              }
            ]
          }
        },
        {
          "name": "task",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  116,
                  97,
                  115,
                  107
                ]
              },
              {
                "kind": "arg",
                "path": "args.task_id"
              }
            ]
          }
        },
        {
          "name": "client",
          "writable": true,
          "signer": true
        },
        {
          "name": "client_covnt",
          "writable": true
        },
        {
          "name": "escrow_vault",
          "writable": true
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "token_program"
        },
        {
          "name": "system_program",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "CreateTaskArgs"
            }
          }
        }
      ]
    },
    {
      "name": "initialize",
      "discriminator": [
        175,
        175,
        109,
        31,
        13,
        152,
        155,
        237
      ],
      "accounts": [
        {
          "name": "config",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "authority",
          "writable": true,
          "signer": true
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "treasury"
        },
        {
          "name": "system_program",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "InitializeArgs"
            }
          }
        }
      ]
    },
    {
      "name": "migrate_config",
      "docs": [
        "One-time migration of a legacy `Config` (predates `min_stake_lock`) to",
        "the current layout: grows the account by 8 bytes and writes the field.",
        "Uses a raw account because the on-chain bytes cannot deserialize into",
        "the new struct until the realloc completes. Authority is checked by",
        "reading the on-chain `authority` field directly. Idempotent: re-running",
        "on a current-layout config just rewrites the value."
      ],
      "discriminator": [
        92,
        131,
        58,
        105,
        210,
        154,
        224,
        193
      ],
      "accounts": [
        {
          "name": "config",
          "docs": [
            "legacy on-chain bytes do not fit the current `Config` layout."
          ],
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "authority",
          "writable": true,
          "signer": true
        },
        {
          "name": "system_program",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "min_stake_lock",
          "type": "u64"
        }
      ]
    },
    {
      "name": "open_credit_account",
      "discriminator": [
        64,
        228,
        64,
        94,
        52,
        139,
        105,
        166
      ],
      "accounts": [
        {
          "name": "credits",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  114,
                  101,
                  100,
                  105,
                  116,
                  115
                ]
              },
              {
                "kind": "account",
                "path": "owner"
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "system_program",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": []
    },
    {
      "name": "refund_task",
      "docs": [
        "Return the escrow to the client. Two paths: the client reclaims after",
        "the deadline passes with no submission (provider never delivered), or",
        "a set arbiter refunds during the review window (dispute resolved in",
        "the client's favour). Pause check matches the release path."
      ],
      "discriminator": [
        8,
        8,
        152,
        190,
        24,
        24,
        158,
        21
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "task",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  116,
                  97,
                  115,
                  107
                ]
              },
              {
                "kind": "account",
                "path": "task.task_id",
                "account": "Task"
              }
            ]
          }
        },
        {
          "name": "authority",
          "docs": [
            "Client (post-deadline reclaim) or arbiter (dispute ruling); verified in the handler."
          ],
          "signer": true
        },
        {
          "name": "escrow_vault",
          "writable": true
        },
        {
          "name": "client_covnt",
          "writable": true
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "token_program"
        }
      ],
      "args": []
    },
    {
      "name": "register_agent",
      "discriminator": [
        135,
        157,
        66,
        195,
        2,
        113,
        175,
        30
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "agent",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  97,
                  103,
                  101,
                  110,
                  116
                ]
              },
              {
                "kind": "arg",
                "path": "args.agent_key"
              }
            ]
          }
        },
        {
          "name": "operator",
          "writable": true,
          "signer": true
        },
        {
          "name": "system_program",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "RegisterAgentArgs"
            }
          }
        }
      ]
    },
    {
      "name": "release_task",
      "docs": [
        "Release the escrow to the provider for delivered work. Signed by the",
        "client (approving the submission) or, when one is set, the arbiter",
        "(resolving a dispute in the provider's favour). Requires a prior",
        "submission, so funds only move against on-chain proof of delivery."
      ],
      "discriminator": [
        189,
        118,
        142,
        99,
        37,
        244,
        38,
        119
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "task",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  116,
                  97,
                  115,
                  107
                ]
              },
              {
                "kind": "account",
                "path": "task.task_id",
                "account": "Task"
              }
            ]
          }
        },
        {
          "name": "authority",
          "docs": [
            "Client (approving) or arbiter (dispute ruling); verified in the handler."
          ],
          "signer": true
        },
        {
          "name": "escrow_vault",
          "writable": true
        },
        {
          "name": "provider_covnt",
          "writable": true
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "token_program"
        }
      ],
      "args": []
    },
    {
      "name": "set_agent_active",
      "discriminator": [
        89,
        168,
        250,
        105,
        254,
        69,
        246,
        221
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "authority",
          "signer": true,
          "relations": [
            "config"
          ]
        },
        {
          "name": "agent",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  97,
                  103,
                  101,
                  110,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "agent.agent_key",
                "account": "Agent"
              }
            ]
          }
        }
      ],
      "args": [
        {
          "name": "active",
          "type": "bool"
        }
      ]
    },
    {
      "name": "set_credits_per_covnt",
      "discriminator": [
        62,
        201,
        222,
        239,
        238,
        198,
        105,
        136
      ],
      "accounts": [
        {
          "name": "config",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "authority",
          "signer": true,
          "relations": [
            "config"
          ]
        }
      ],
      "args": [
        {
          "name": "credits_per_covnt",
          "type": "u64"
        }
      ]
    },
    {
      "name": "set_min_stake_lock",
      "discriminator": [
        29,
        59,
        61,
        151,
        87,
        232,
        173,
        143
      ],
      "accounts": [
        {
          "name": "config",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "authority",
          "signer": true,
          "relations": [
            "config"
          ]
        }
      ],
      "args": [
        {
          "name": "min_stake_lock",
          "type": "u64"
        }
      ]
    },
    {
      "name": "set_pause",
      "discriminator": [
        63,
        32,
        154,
        2,
        56,
        103,
        79,
        45
      ],
      "accounts": [
        {
          "name": "config",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "authority",
          "signer": true,
          "relations": [
            "config"
          ]
        }
      ],
      "args": [
        {
          "name": "paused",
          "type": "bool"
        }
      ]
    },
    {
      "name": "slash_stake",
      "discriminator": [
        190,
        242,
        137,
        27,
        41,
        18,
        233,
        37
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "slash_authority",
          "signer": true,
          "relations": [
            "config"
          ]
        },
        {
          "name": "agent",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  97,
                  103,
                  101,
                  110,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "agent.agent_key",
                "account": "Agent"
              }
            ]
          }
        },
        {
          "name": "position",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  116,
                  97,
                  107,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "position.agent_key",
                "account": "StakePosition"
              },
              {
                "kind": "account",
                "path": "position.owner",
                "account": "StakePosition"
              }
            ]
          }
        },
        {
          "name": "stake_vault",
          "writable": true
        },
        {
          "name": "slash_vault",
          "writable": true
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "token_program"
        }
      ],
      "args": [
        {
          "name": "amount",
          "type": "u64"
        },
        {
          "name": "reason_hash",
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
      "name": "stake",
      "discriminator": [
        206,
        176,
        202,
        18,
        200,
        209,
        179,
        108
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "agent",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  97,
                  103,
                  101,
                  110,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "agent.agent_key",
                "account": "Agent"
              }
            ]
          }
        },
        {
          "name": "position",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  116,
                  97,
                  107,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "agent.agent_key",
                "account": "Agent"
              },
              {
                "kind": "account",
                "path": "owner"
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "owner_covnt",
          "writable": true
        },
        {
          "name": "stake_vault",
          "writable": true
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "token_program"
        },
        {
          "name": "system_program",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "amount",
          "type": "u64"
        },
        {
          "name": "lock_until",
          "type": "u64"
        }
      ]
    },
    {
      "name": "submit_task",
      "docs": [
        "The provider posts proof of delivery on-chain, moving the task from",
        "FUNDED to SUBMITTED and starting the review window. Only the named",
        "provider may submit, and only before the deadline, so a late delivery",
        "cannot settle out from under the client's refund right."
      ],
      "discriminator": [
        148,
        183,
        26,
        116,
        107,
        213,
        118,
        213
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "task",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  116,
                  97,
                  115,
                  107
                ]
              },
              {
                "kind": "account",
                "path": "task.task_id",
                "account": "Task"
              }
            ]
          }
        },
        {
          "name": "provider",
          "signer": true
        }
      ],
      "args": [
        {
          "name": "result_hash",
          "type": {
            "array": [
              "u8",
              32
            ]
          }
        },
        {
          "name": "receipt_hash",
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
      "name": "unstake",
      "docs": [
        "Owner-signed withdrawal of a staked position once `lock_until`",
        "has passed. Transfers the full position balance back to the",
        "owner's COVNT account, decrements `agent.stake`, and closes the",
        "position account (rent returned to the owner). Closing frees the",
        "canonical `[b\"stake\", agent_key, owner]` PDA so the owner can",
        "re-stake against the same agent afterwards."
      ],
      "discriminator": [
        90,
        95,
        107,
        42,
        205,
        124,
        50,
        225
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "agent",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  97,
                  103,
                  101,
                  110,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "position.agent_key",
                "account": "StakePosition"
              }
            ]
          }
        },
        {
          "name": "position",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  116,
                  97,
                  107,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "position.agent_key",
                "account": "StakePosition"
              },
              {
                "kind": "account",
                "path": "position.owner",
                "account": "StakePosition"
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "stake_vault",
          "writable": true
        },
        {
          "name": "owner_covnt",
          "writable": true
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "token_program"
        }
      ],
      "args": []
    },
    {
      "name": "update_authority",
      "discriminator": [
        32,
        46,
        64,
        28,
        149,
        75,
        243,
        88
      ],
      "accounts": [
        {
          "name": "config",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "authority",
          "signer": true,
          "relations": [
            "config"
          ]
        }
      ],
      "args": [
        {
          "name": "new_authority",
          "type": "pubkey"
        }
      ]
    },
    {
      "name": "update_slash_authority",
      "discriminator": [
        78,
        88,
        227,
        85,
        178,
        109,
        118,
        199
      ],
      "accounts": [
        {
          "name": "config",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "authority",
          "signer": true,
          "relations": [
            "config"
          ]
        }
      ],
      "args": [
        {
          "name": "new_slash_authority",
          "type": "pubkey"
        }
      ]
    },
    {
      "name": "update_treasury",
      "discriminator": [
        60,
        16,
        243,
        66,
        96,
        59,
        254,
        131
      ],
      "accounts": [
        {
          "name": "config",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  99,
                  111,
                  110,
                  102,
                  105,
                  103
                ]
              }
            ]
          }
        },
        {
          "name": "authority",
          "signer": true,
          "relations": [
            "config"
          ]
        },
        {
          "name": "treasury"
        }
      ],
      "args": []
    }
  ],
  "accounts": [
    {
      "name": "Agent",
      "discriminator": [
        47,
        166,
        112,
        147,
        155,
        197,
        86,
        7
      ]
    },
    {
      "name": "Config",
      "discriminator": [
        155,
        12,
        170,
        224,
        30,
        250,
        204,
        130
      ]
    },
    {
      "name": "CreditAccount",
      "discriminator": [
        196,
        171,
        234,
        132,
        239,
        255,
        21,
        96
      ]
    },
    {
      "name": "ReceiptBatch",
      "discriminator": [
        234,
        250,
        48,
        59,
        242,
        148,
        55,
        76
      ]
    },
    {
      "name": "StakePosition",
      "discriminator": [
        78,
        165,
        30,
        111,
        171,
        125,
        11,
        220
      ]
    },
    {
      "name": "Task",
      "discriminator": [
        79,
        34,
        229,
        55,
        88,
        90,
        55,
        84
      ]
    }
  ],
  "events": [
    {
      "name": "AgentRegistered",
      "discriminator": [
        191,
        78,
        217,
        54,
        232,
        100,
        189,
        85
      ]
    },
    {
      "name": "AgentStatusUpdated",
      "discriminator": [
        196,
        209,
        177,
        67,
        67,
        223,
        225,
        10
      ]
    },
    {
      "name": "AuthorityUpdated",
      "discriminator": [
        133,
        207,
        24,
        122,
        14,
        234,
        91,
        34
      ]
    },
    {
      "name": "ConfigMigrated",
      "discriminator": [
        115,
        69,
        99,
        100,
        192,
        77,
        40,
        50
      ]
    },
    {
      "name": "CovntBurned",
      "discriminator": [
        13,
        107,
        115,
        161,
        149,
        126,
        111,
        157
      ]
    },
    {
      "name": "CreditAccountOpened",
      "discriminator": [
        49,
        4,
        136,
        30,
        100,
        84,
        241,
        163
      ]
    },
    {
      "name": "CreditsConsumed",
      "discriminator": [
        220,
        125,
        34,
        34,
        123,
        171,
        155,
        83
      ]
    },
    {
      "name": "CreditsPurchased",
      "discriminator": [
        176,
        67,
        39,
        167,
        11,
        116,
        222,
        22
      ]
    },
    {
      "name": "CreditsRateUpdated",
      "discriminator": [
        228,
        74,
        133,
        174,
        6,
        112,
        59,
        239
      ]
    },
    {
      "name": "MinStakeLockUpdated",
      "discriminator": [
        252,
        51,
        29,
        123,
        233,
        95,
        131,
        171
      ]
    },
    {
      "name": "ProtocolInitialized",
      "discriminator": [
        173,
        122,
        168,
        254,
        9,
        118,
        76,
        132
      ]
    },
    {
      "name": "ProtocolPauseUpdated",
      "discriminator": [
        18,
        112,
        97,
        19,
        182,
        70,
        162,
        226
      ]
    },
    {
      "name": "ReceiptBatchAnchored",
      "discriminator": [
        127,
        15,
        203,
        26,
        234,
        96,
        102,
        135
      ]
    },
    {
      "name": "SlashAuthorityUpdated",
      "discriminator": [
        155,
        152,
        63,
        163,
        22,
        24,
        2,
        23
      ]
    },
    {
      "name": "StakeOpened",
      "discriminator": [
        224,
        249,
        211,
        227,
        9,
        83,
        142,
        188
      ]
    },
    {
      "name": "StakePositionClosed",
      "discriminator": [
        170,
        89,
        137,
        137,
        112,
        27,
        94,
        212
      ]
    },
    {
      "name": "StakeSlashed",
      "discriminator": [
        43,
        41,
        196,
        25,
        218,
        235,
        244,
        35
      ]
    },
    {
      "name": "StakeWithdrawn",
      "discriminator": [
        33,
        120,
        159,
        58,
        140,
        255,
        174,
        79
      ]
    },
    {
      "name": "TaskClaimed",
      "discriminator": [
        208,
        90,
        243,
        116,
        80,
        15,
        228,
        202
      ]
    },
    {
      "name": "TaskCreated",
      "discriminator": [
        49,
        174,
        6,
        7,
        71,
        159,
        69,
        175
      ]
    },
    {
      "name": "TaskRefunded",
      "discriminator": [
        49,
        230,
        123,
        76,
        62,
        16,
        104,
        128
      ]
    },
    {
      "name": "TaskReleased",
      "discriminator": [
        206,
        87,
        144,
        39,
        45,
        254,
        186,
        247
      ]
    },
    {
      "name": "TaskSubmitted",
      "discriminator": [
        39,
        29,
        92,
        117,
        184,
        101,
        14,
        126
      ]
    },
    {
      "name": "TreasuryUpdated",
      "discriminator": [
        80,
        239,
        54,
        168,
        43,
        38,
        85,
        145
      ]
    }
  ],
  "errors": [
    {
      "code": 6000,
      "name": "ZeroAmount",
      "msg": "amount must be greater than zero"
    },
    {
      "code": 6001,
      "name": "Overflow",
      "msg": "arithmetic overflow"
    },
    {
      "code": 6002,
      "name": "ProtocolPaused",
      "msg": "protocol is paused"
    },
    {
      "code": 6003,
      "name": "Unauthorized",
      "msg": "unauthorized"
    },
    {
      "code": 6004,
      "name": "WrongMint",
      "msg": "wrong COVNT mint"
    },
    {
      "code": 6005,
      "name": "AgentInactive",
      "msg": "agent is inactive"
    },
    {
      "code": 6006,
      "name": "AgentMismatch",
      "msg": "agent mismatch"
    },
    {
      "code": 6007,
      "name": "InsufficientCredits",
      "msg": "insufficient credits"
    },
    {
      "code": 6008,
      "name": "InsufficientStake",
      "msg": "insufficient stake"
    },
    {
      "code": 6009,
      "name": "StakeInactive",
      "msg": "stake position is inactive"
    },
    {
      "code": 6010,
      "name": "WrongTaskStatus",
      "msg": "wrong task status"
    },
    {
      "code": 6011,
      "name": "TaskExpired",
      "msg": "task deadline has passed; release no longer allowed (use refund_task)"
    },
    {
      "code": 6012,
      "name": "TaskNotExpired",
      "msg": "task deadline has not passed; refund not yet available"
    },
    {
      "code": 6013,
      "name": "StakeLocked",
      "msg": "stake position is still locked"
    },
    {
      "code": 6014,
      "name": "StakeStillActive",
      "msg": "stake position is still active; unstake before closing"
    },
    {
      "code": 6015,
      "name": "LockTooShort",
      "msg": "lock_until is shorter than the protocol minimum stake lock"
    },
    {
      "code": 6016,
      "name": "NotArbiter",
      "msg": "only the task arbiter may take this action"
    },
    {
      "code": 6017,
      "name": "ReviewWindowNotElapsed",
      "msg": "the review window has not elapsed yet; provider cannot claim"
    },
    {
      "code": 6018,
      "name": "ReviewWindowElapsed",
      "msg": "the review window has elapsed; arbiter refund no longer allowed"
    },
    {
      "code": 6019,
      "name": "InvalidReviewWindow",
      "msg": "review window must be zero or positive"
    },
    {
      "code": 6020,
      "name": "InvalidDeadline",
      "msg": "deadline must be in the future"
    }
  ],
  "types": [
    {
      "name": "Agent",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "agent_key",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "operator",
            "type": "pubkey"
          },
          {
            "name": "metadata_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "capability_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "stake",
            "type": "u64"
          },
          {
            "name": "reputation",
            "type": "u64"
          },
          {
            "name": "active",
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
      "name": "AgentRegistered",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "agent_key",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "operator",
            "type": "pubkey"
          },
          {
            "name": "metadata_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "capability_hash",
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
      "name": "AgentStatusUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "agent_key",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "active",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "AnchorReceiptBatchArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "batch_id",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "merkle_root",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "receipt_count",
            "type": "u32"
          }
        ]
      }
    },
    {
      "name": "AuthorityUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "previous",
            "type": "pubkey"
          },
          {
            "name": "new_authority",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "Config",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "slash_authority",
            "type": "pubkey"
          },
          {
            "name": "covnt_mint",
            "type": "pubkey"
          },
          {
            "name": "treasury",
            "type": "pubkey"
          },
          {
            "name": "credits_per_covnt",
            "type": "u64"
          },
          {
            "name": "paused",
            "type": "bool"
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "min_stake_lock",
            "docs": [
              "Minimum seconds a stake must remain locked past the staking instant.",
              "`0` disables the floor (a staker may pick any `lock_until`). Appended",
              "last so legacy 146-byte configs migrate by realloc (see `migrate_config`)."
            ],
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "ConfigMigrated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "min_stake_lock",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "CovntBurned",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "reason_hash",
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
      "name": "CreateTaskArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "task_id",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "provider",
            "type": "pubkey"
          },
          {
            "name": "arbiter",
            "type": "pubkey"
          },
          {
            "name": "amount_covnt",
            "type": "u64"
          },
          {
            "name": "task_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "criteria_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "deadline",
            "type": "i64"
          },
          {
            "name": "review_window",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "CreditAccount",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "balance",
            "type": "u64"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "CreditAccountOpened",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "credit_account",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "CreditsConsumed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "receipt_hash",
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
      "name": "CreditsPurchased",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "amount_covnt",
            "type": "u64"
          },
          {
            "name": "credits",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "CreditsRateUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "previous",
            "type": "u64"
          },
          {
            "name": "credits_per_covnt",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "InitializeArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "slash_authority",
            "type": "pubkey"
          },
          {
            "name": "credits_per_covnt",
            "type": "u64"
          },
          {
            "name": "min_stake_lock",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "MinStakeLockUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "previous",
            "type": "u64"
          },
          {
            "name": "min_stake_lock",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "ProtocolInitialized",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "slash_authority",
            "type": "pubkey"
          },
          {
            "name": "covnt_mint",
            "type": "pubkey"
          },
          {
            "name": "treasury",
            "type": "pubkey"
          },
          {
            "name": "credits_per_covnt",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "ProtocolPauseUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "paused",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "ReceiptBatch",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "batch_id",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "merkle_root",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "receipt_count",
            "type": "u32"
          },
          {
            "name": "created_at",
            "type": "i64"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "ReceiptBatchAnchored",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "batch_id",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "merkle_root",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "receipt_count",
            "type": "u32"
          },
          {
            "name": "created_at",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "RegisterAgentArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "agent_key",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "metadata_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "capability_hash",
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
      "name": "SlashAuthorityUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "previous",
            "type": "pubkey"
          },
          {
            "name": "new_slash_authority",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "StakeOpened",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "agent_key",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "lock_until",
            "type": "u64"
          },
          {
            "name": "position",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "StakePosition",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "agent_key",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "lock_until",
            "type": "u64"
          },
          {
            "name": "active",
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
      "name": "StakePositionClosed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "agent_key",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "owner",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "StakeSlashed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "agent_key",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "reason_hash",
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
      "name": "StakeWithdrawn",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "agent_key",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "withdrawn_at",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "Task",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "task_id",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "client",
            "type": "pubkey"
          },
          {
            "name": "agent_key",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "provider",
            "type": "pubkey"
          },
          {
            "name": "arbiter",
            "docs": [
              "Neutral dispute resolver. `Pubkey::default()` means no arbiter, in",
              "which case delivered work settles to the provider after the review",
              "window and the client's only recourse is a pre-submission refund."
            ],
            "type": "pubkey"
          },
          {
            "name": "amount_covnt",
            "type": "u64"
          },
          {
            "name": "task_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "criteria_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "result_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "receipt_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "deadline",
            "docs": [
              "Submission deadline. The provider must submit by this time or the",
              "client can reclaim the escrow."
            ],
            "type": "i64"
          },
          {
            "name": "submitted_at",
            "docs": [
              "Set when the provider submits; 0 while FUNDED."
            ],
            "type": "i64"
          },
          {
            "name": "review_window",
            "docs": [
              "Seconds after submission before the provider may self-claim."
            ],
            "type": "i64"
          },
          {
            "name": "status",
            "type": "u8"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "TaskClaimed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "task_id",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "provider",
            "type": "pubkey"
          },
          {
            "name": "amount_covnt",
            "type": "u64"
          },
          {
            "name": "claimed_at",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "TaskCreated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "task_id",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "client",
            "type": "pubkey"
          },
          {
            "name": "agent_key",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "provider",
            "type": "pubkey"
          },
          {
            "name": "arbiter",
            "type": "pubkey"
          },
          {
            "name": "amount_covnt",
            "type": "u64"
          },
          {
            "name": "task_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "criteria_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "deadline",
            "type": "i64"
          },
          {
            "name": "review_window",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "TaskRefunded",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "task_id",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "client",
            "type": "pubkey"
          },
          {
            "name": "amount_covnt",
            "type": "u64"
          },
          {
            "name": "deadline",
            "type": "i64"
          },
          {
            "name": "refunded_at",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "TaskReleased",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "task_id",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "provider",
            "type": "pubkey"
          },
          {
            "name": "amount_covnt",
            "type": "u64"
          },
          {
            "name": "result_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "receipt_hash",
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
      "name": "TaskSubmitted",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "task_id",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "provider",
            "type": "pubkey"
          },
          {
            "name": "submitted_at",
            "type": "i64"
          },
          {
            "name": "result_hash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "receipt_hash",
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
      "name": "TreasuryUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "previous",
            "type": "pubkey"
          },
          {
            "name": "new_treasury",
            "type": "pubkey"
          }
        ]
      }
    }
  ]
};
