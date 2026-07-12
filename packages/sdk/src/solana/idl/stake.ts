// Generated from the on-chain program IDL — do not edit by hand.
// Anchor IDL for the covenant stake program (CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED).
export const stakeIdl = {
  "address": "CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED",
  "metadata": {
    "name": "stake",
    "version": "0.1.0",
    "spec": "0.1.0",
    "description": "Covenant Solana protocol program for $CVNT lock-tiered staking with SOL fee distribution and buy-and-lock treasury."
  },
  "instructions": [
    {
      "name": "accrue",
      "discriminator": [
        23,
        76,
        128,
        149,
        229,
        247,
        72,
        228
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
        }
      ],
      "args": []
    },
    {
      "name": "claim",
      "discriminator": [
        62,
        198,
        214,
        193,
        213,
        159,
        108,
        210
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
                  101,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "owner"
              },
              {
                "kind": "account",
                "path": "position.nonce",
                "account": "StakePosition"
              }
            ]
          }
        },
        {
          "name": "reward_vault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  114,
                  101,
                  119,
                  97,
                  114,
                  100,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true,
          "relations": [
            "position"
          ]
        }
      ],
      "args": []
    },
    {
      "name": "close_position",
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
          "name": "config",
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
                  101,
                  95,
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
                  101,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "owner"
              },
              {
                "kind": "account",
                "path": "position.nonce",
                "account": "StakePosition"
              }
            ]
          }
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "locked_vault_authority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  118,
                  97,
                  117,
                  108,
                  116,
                  95,
                  97,
                  117,
                  116,
                  104
                ]
              }
            ]
          }
        },
        {
          "name": "locked_cvnt_vault",
          "writable": true
        },
        {
          "name": "owner_cvnt_ata",
          "writable": true
        },
        {
          "name": "reward_vault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  114,
                  101,
                  119,
                  97,
                  114,
                  100,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true,
          "relations": [
            "position"
          ]
        },
        {
          "name": "token_program"
        }
      ],
      "args": []
    },
    {
      "name": "create_position",
      "discriminator": [
        48,
        215,
        197,
        153,
        96,
        203,
        180,
        133
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
                  101,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "owner"
              },
              {
                "kind": "arg",
                "path": "args.nonce"
              }
            ]
          }
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "locked_vault_authority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  118,
                  97,
                  117,
                  108,
                  116,
                  95,
                  97,
                  117,
                  116,
                  104
                ]
              }
            ]
          }
        },
        {
          "name": "locked_cvnt_vault",
          "writable": true
        },
        {
          "name": "owner_cvnt_ata",
          "writable": true
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
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
              "name": "CreatePositionArgs"
            }
          }
        }
      ]
    },
    {
      "name": "deposit_buylock_cvnt",
      "discriminator": [
        123,
        96,
        0,
        227,
        193,
        32,
        181,
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
          "name": "fee_router",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  101,
                  101,
                  95,
                  114,
                  111,
                  117,
                  116,
                  101,
                  114
                ]
              }
            ]
          }
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "buylock_vault_authority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  117,
                  121,
                  108,
                  111,
                  99,
                  107,
                  95,
                  97,
                  117,
                  116,
                  104
                ]
              }
            ]
          }
        },
        {
          "name": "buylock_cvnt_vault",
          "writable": true
        },
        {
          "name": "depositor_cvnt_ata",
          "writable": true
        },
        {
          "name": "depositor",
          "writable": true,
          "signer": true
        },
        {
          "name": "token_program"
        }
      ],
      "args": [
        {
          "name": "amount",
          "type": "u64"
        }
      ]
    },
    {
      "name": "deposit_sol_fees",
      "discriminator": [
        177,
        47,
        101,
        11,
        249,
        203,
        253,
        219
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
          "name": "fee_router",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  101,
                  101,
                  95,
                  114,
                  111,
                  117,
                  116,
                  101,
                  114
                ]
              }
            ]
          }
        },
        {
          "name": "reward_vault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  114,
                  101,
                  119,
                  97,
                  114,
                  100,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              }
            ]
          }
        },
        {
          "name": "depositor",
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
          "name": "amount",
          "type": "u64"
        }
      ]
    },
    {
      "name": "increase_amount",
      "discriminator": [
        128,
        251,
        247,
        1,
        206,
        93,
        128,
        59
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
                  101,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "owner"
              },
              {
                "kind": "account",
                "path": "position.nonce",
                "account": "StakePosition"
              }
            ]
          }
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "locked_vault_authority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  118,
                  97,
                  117,
                  108,
                  116,
                  95,
                  97,
                  117,
                  116,
                  104
                ]
              }
            ]
          }
        },
        {
          "name": "locked_cvnt_vault",
          "writable": true
        },
        {
          "name": "owner_cvnt_ata",
          "writable": true
        },
        {
          "name": "owner",
          "signer": true,
          "relations": [
            "position"
          ]
        },
        {
          "name": "token_program"
        }
      ],
      "args": [
        {
          "name": "extra",
          "type": "u64"
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
          "name": "fee_router",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  101,
                  101,
                  95,
                  114,
                  111,
                  117,
                  116,
                  101,
                  114
                ]
              }
            ]
          }
        },
        {
          "name": "locked_vault_authority",
          "docs": [
            "PDA signer for the locked-CVNT vault ATA. No data, derived only."
          ],
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  118,
                  97,
                  117,
                  108,
                  116,
                  95,
                  97,
                  117,
                  116,
                  104
                ]
              }
            ]
          }
        },
        {
          "name": "reward_vault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  114,
                  101,
                  119,
                  97,
                  114,
                  100,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              }
            ]
          }
        },
        {
          "name": "buylock_vault_authority",
          "docs": [
            "PDA signer for the BuyLock CVNT vault ATA. No data, derived only."
          ],
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  117,
                  121,
                  108,
                  111,
                  99,
                  107,
                  95,
                  97,
                  117,
                  116,
                  104
                ]
              }
            ]
          }
        },
        {
          "name": "covnt_mint"
        },
        {
          "name": "locked_cvnt_vault",
          "docs": [
            "Locked-CVNT vault: ATA owned by `locked_vault_authority`. Must be created",
            "by the caller (use Associated Token Program create_idempotent first)."
          ]
        },
        {
          "name": "buylock_cvnt_vault",
          "docs": [
            "BuyLock CVNT vault: ATA owned by `buylock_vault_authority`. Must be",
            "created by the caller."
          ]
        },
        {
          "name": "authority",
          "writable": true,
          "signer": true
        },
        {
          "name": "program_data",
          "docs": [
            "`verify_upgrade_authority` against the bpf_loader_upgradeable program",
            "owner + derived PDA + the upgrade_authority field in the layout."
          ]
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
              "name": "InitializeArgs"
            }
          }
        }
      ]
    },
    {
      "name": "pause",
      "discriminator": [
        211,
        22,
        221,
        251,
        74,
        121,
        193,
        47
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
          "name": "signer",
          "signer": true
        }
      ],
      "args": []
    },
    {
      "name": "rotate_fee_router",
      "discriminator": [
        79,
        190,
        88,
        235,
        146,
        120,
        69,
        241
      ],
      "accounts": [
        {
          "name": "config",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
          "name": "fee_router",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  101,
                  101,
                  95,
                  114,
                  111,
                  117,
                  116,
                  101,
                  114
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
          "name": "args",
          "type": {
            "defined": {
              "name": "RotateFeeRouterArgs"
            }
          }
        }
      ]
    },
    {
      "name": "unpause",
      "discriminator": [
        169,
        144,
        4,
        38,
        10,
        141,
        188,
        255
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
      "name": "update_max_active_locks",
      "discriminator": [
        15,
        232,
        41,
        79,
        10,
        29,
        205,
        41
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
          "name": "new_max",
          "type": "u32"
        }
      ]
    },
    {
      "name": "update_min_lock_amount",
      "discriminator": [
        248,
        172,
        183,
        228,
        230,
        38,
        82,
        99
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
                  115,
                  116,
                  97,
                  107,
                  101,
                  95,
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
          "name": "new_min",
          "type": "u64"
        }
      ]
    }
  ],
  "accounts": [
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
      "name": "FeeRouter",
      "discriminator": [
        180,
        161,
        131,
        96,
        121,
        91,
        71,
        183
      ]
    },
    {
      "name": "RewardVault",
      "discriminator": [
        201,
        22,
        221,
        167,
        208,
        16,
        210,
        33
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
    }
  ],
  "events": [
    {
      "name": "AccrualTick",
      "discriminator": [
        54,
        92,
        230,
        73,
        95,
        234,
        239,
        81
      ]
    },
    {
      "name": "AuthorityRotated",
      "discriminator": [
        89,
        124,
        120,
        223,
        3,
        19,
        185,
        230
      ]
    },
    {
      "name": "BuyLockDeposited",
      "discriminator": [
        156,
        64,
        12,
        28,
        58,
        177,
        78,
        214
      ]
    },
    {
      "name": "FeeRouterRotated",
      "discriminator": [
        139,
        253,
        48,
        11,
        62,
        147,
        159,
        157
      ]
    },
    {
      "name": "MaxActiveLocksUpdated",
      "discriminator": [
        223,
        172,
        57,
        66,
        87,
        244,
        84,
        52
      ]
    },
    {
      "name": "MinLockAmountUpdated",
      "discriminator": [
        93,
        173,
        87,
        101,
        25,
        107,
        1,
        205
      ]
    },
    {
      "name": "PositionClosed",
      "discriminator": [
        157,
        163,
        227,
        228,
        13,
        97,
        138,
        121
      ]
    },
    {
      "name": "PositionCreated",
      "discriminator": [
        63,
        226,
        54,
        63,
        141,
        22,
        31,
        221
      ]
    },
    {
      "name": "PositionIncreased",
      "discriminator": [
        73,
        58,
        247,
        181,
        100,
        237,
        249,
        81
      ]
    },
    {
      "name": "ProgramInitialized",
      "discriminator": [
        43,
        70,
        110,
        241,
        199,
        218,
        221,
        245
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
      "name": "RewardsClaimed",
      "discriminator": [
        75,
        98,
        88,
        18,
        219,
        112,
        88,
        121
      ]
    },
    {
      "name": "SolFeesDeposited",
      "discriminator": [
        232,
        159,
        229,
        149,
        74,
        71,
        37,
        138
      ]
    }
  ],
  "errors": [
    {
      "code": 6000,
      "name": "ProtocolPaused",
      "msg": "protocol is paused"
    },
    {
      "code": 6001,
      "name": "LockNotExpired",
      "msg": "lock period has not yet expired"
    },
    {
      "code": 6002,
      "name": "LockExpired",
      "msg": "lock period has already expired"
    },
    {
      "code": 6003,
      "name": "InvalidLockTier",
      "msg": "lock tier must be 5000, 10000, 15000, or 20000 bps"
    },
    {
      "code": 6004,
      "name": "LockAmountTooSmall",
      "msg": "lock principal below configured minimum"
    },
    {
      "code": 6005,
      "name": "LockCapExceeded",
      "msg": "max active lock cap exceeded"
    },
    {
      "code": 6006,
      "name": "UnauthorizedAuthority",
      "msg": "signer is not the configured authority"
    },
    {
      "code": 6007,
      "name": "UnauthorizedFeeRouter",
      "msg": "signer is not the configured fee router authority"
    },
    {
      "code": 6008,
      "name": "RateLimited",
      "msg": "rate-limited: too soon after last deposit"
    },
    {
      "code": 6009,
      "name": "DepositAmountExceeded",
      "msg": "deposit exceeds per-call max"
    },
    {
      "code": 6010,
      "name": "MathOverflow",
      "msg": "reward math overflow"
    },
    {
      "code": 6011,
      "name": "InvalidMint",
      "msg": "mint mismatch with config"
    },
    {
      "code": 6012,
      "name": "InsufficientRewardVault",
      "msg": "reward vault has insufficient lamports above rent floor"
    },
    {
      "code": 6013,
      "name": "NothingToClaim",
      "msg": "nothing to claim"
    },
    {
      "code": 6014,
      "name": "ZeroAmount",
      "msg": "zero amount"
    },
    {
      "code": 6015,
      "name": "InvalidParameter",
      "msg": "invalid parameter"
    },
    {
      "code": 6016,
      "name": "NoActiveStakers",
      "msg": "no active stakers — fees would orphan"
    },
    {
      "code": 6017,
      "name": "InvalidMintDecimals",
      "msg": "covnt mint decimals must equal 6"
    }
  ],
  "types": [
    {
      "name": "AccrualTick",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "folded_lamports",
            "type": "u64"
          },
          {
            "name": "acc_sol_per_weight",
            "type": "u128"
          },
          {
            "name": "total_weight",
            "type": "u128"
          }
        ]
      }
    },
    {
      "name": "AuthorityRotated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "old",
            "type": "pubkey"
          },
          {
            "name": "new",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "BuyLockDeposited",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "depositor",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "cumulative_buylock_cvnt",
            "type": "u64"
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
            "name": "pause_authority",
            "type": "pubkey"
          },
          {
            "name": "paused",
            "type": "bool"
          },
          {
            "name": "covnt_mint",
            "type": "pubkey"
          },
          {
            "name": "min_lock_amount",
            "type": "u64"
          },
          {
            "name": "max_active_locks",
            "type": "u32"
          },
          {
            "name": "active_lock_count",
            "type": "u32"
          },
          {
            "name": "acc_sol_per_weight",
            "type": "u128"
          },
          {
            "name": "total_weight",
            "type": "u128"
          },
          {
            "name": "pending_sol_lamports",
            "type": "u64"
          },
          {
            "name": "last_accrual_ts",
            "type": "i64"
          },
          {
            "name": "cumulative_sol_distributed",
            "type": "u64"
          },
          {
            "name": "cumulative_buylock_cvnt",
            "type": "u64"
          },
          {
            "name": "initialized_ts",
            "type": "i64"
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "locked_vault_authority_bump",
            "type": "u8"
          },
          {
            "name": "reward_vault_bump",
            "type": "u8"
          },
          {
            "name": "buylock_vault_authority_bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "CreatePositionArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "nonce",
            "type": "u64"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "lock_tier_bps",
            "type": "u16"
          }
        ]
      }
    },
    {
      "name": "FeeRouter",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "max_deposit_lamports",
            "type": "u64"
          },
          {
            "name": "rate_limit_secs",
            "type": "i64"
          },
          {
            "name": "last_deposit_ts",
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
      "name": "FeeRouterRotated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "old_authority",
            "type": "pubkey"
          },
          {
            "name": "new_authority",
            "type": "pubkey"
          },
          {
            "name": "max_deposit_lamports",
            "type": "u64"
          },
          {
            "name": "rate_limit_secs",
            "type": "i64"
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
            "name": "pause_authority",
            "type": "pubkey"
          },
          {
            "name": "fee_router_authority",
            "type": "pubkey"
          },
          {
            "name": "min_lock_amount",
            "type": "u64"
          },
          {
            "name": "fee_router_max_deposit_lamports",
            "type": "u64"
          },
          {
            "name": "fee_router_rate_limit_secs",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "MaxActiveLocksUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "old",
            "type": "u32"
          },
          {
            "name": "new",
            "type": "u32"
          }
        ]
      }
    },
    {
      "name": "MinLockAmountUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "old",
            "type": "u64"
          },
          {
            "name": "new",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "PositionClosed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "nonce",
            "type": "u64"
          },
          {
            "name": "returned_amount",
            "type": "u64"
          },
          {
            "name": "paid_lamports",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "PositionCreated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "nonce",
            "type": "u64"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "weight",
            "type": "u128"
          },
          {
            "name": "multiplier_bps",
            "type": "u16"
          },
          {
            "name": "lock_end",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "PositionIncreased",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "nonce",
            "type": "u64"
          },
          {
            "name": "extra",
            "type": "u64"
          },
          {
            "name": "new_amount",
            "type": "u64"
          },
          {
            "name": "new_weight",
            "type": "u128"
          },
          {
            "name": "settled_pending_lamports",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "ProgramInitialized",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "pause_authority",
            "type": "pubkey"
          },
          {
            "name": "fee_router_authority",
            "type": "pubkey"
          },
          {
            "name": "covnt_mint",
            "type": "pubkey"
          },
          {
            "name": "min_lock_amount",
            "type": "u64"
          },
          {
            "name": "max_active_locks",
            "type": "u32"
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
          },
          {
            "name": "by",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "RewardVault",
      "type": {
        "kind": "struct",
        "fields": []
      }
    },
    {
      "name": "RewardsClaimed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "nonce",
            "type": "u64"
          },
          {
            "name": "lamports",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "RotateFeeRouterArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "new_authority",
            "type": {
              "option": "pubkey"
            }
          },
          {
            "name": "new_max_deposit_lamports",
            "type": {
              "option": "u64"
            }
          },
          {
            "name": "new_rate_limit_secs",
            "type": {
              "option": "i64"
            }
          }
        ]
      }
    },
    {
      "name": "SolFeesDeposited",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "depositor",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "pending_after",
            "type": "u64"
          },
          {
            "name": "acc_sol_per_weight",
            "type": "u128"
          },
          {
            "name": "total_weight",
            "type": "u128"
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
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "nonce",
            "type": "u64"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "weight",
            "type": "u128"
          },
          {
            "name": "multiplier_bps",
            "type": "u16"
          },
          {
            "name": "lock_start",
            "type": "i64"
          },
          {
            "name": "lock_end",
            "type": "i64"
          },
          {
            "name": "reward_debt",
            "type": "u128"
          },
          {
            "name": "unclaimed_lamports",
            "type": "u64"
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
    }
  ]
};
