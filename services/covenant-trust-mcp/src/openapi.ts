export const openApiDocument = {
  openapi: '3.1.0',
  info: {
    title: 'Covenant Trust',
    version: '0.2.0',
    description:
      'Catalog-neutral identity, transfer-history, and attestation facts for agents. ' +
      'The API reports evidence and coverage; callers apply their own allow, review, or deny policy.',
  },
  tags: [
    {name: 'Identity'},
    {name: 'Payment history'},
    {name: 'Attestations'},
  ],
  paths: {
    '/health': {
      get: {
        operationId: 'health',
        summary: 'Service health',
        responses: {
          '200': {
            description: 'Healthy',
            content: {
              'application/json': {
                schema: {$ref: '#/components/schemas/Health'},
              },
            },
          },
        },
      },
    },
    '/v1/payment-history/{wallet}': {
      get: {
        operationId: 'getObservedPaymentHistory',
        summary: 'Get coverage-limited PayAI-sponsored transfer history',
        description:
          'Scans a bounded window of recent PayAI fee-payer transactions for inbound USDC transfers. ' +
          'Fee sponsorship does not prove an x402 request, settlement receipt, completed job, or reputation.',
        tags: ['Payment history'],
        parameters: [
          {
            name: 'wallet',
            in: 'path',
            required: true,
            schema: {type: 'string'},
            description: 'Solana wallet address',
          },
        ],
        responses: {
          '200': {
            description: 'Observed transfer facts and explicit scan coverage',
            content: {
              'application/json': {
                schema: {$ref: '#/components/schemas/PaymentHistory'},
              },
            },
          },
          '400': {$ref: '#/components/responses/BadRequest'},
          '503': {$ref: '#/components/responses/Unavailable'},
        },
      },
    },
    '/v1/agents/{asset}': {
      get: {
        operationId: 'getAgentPassport',
        summary: 'Get agent identity and validation facts',
        description:
          'Reads an MPL Core asset, its Agent Identity registry PDA, a bounded scan for the proposed ' +
          'Covenant validation profile, and the explicitly named deployed legacy record. On-chain URIs ' +
          'are returned as untrusted strings and are not fetched.',
        tags: ['Identity'],
        parameters: [
          {
            name: 'asset',
            in: 'path',
            required: true,
            schema: {type: 'string'},
            description: 'MPL Core asset address',
          },
        ],
        responses: {
          '200': {
            description: 'Agent passport',
            content: {
              'application/json': {
                schema: {$ref: '#/components/schemas/AgentPassport'},
              },
            },
          },
          '400': {$ref: '#/components/responses/BadRequest'},
          '404': {$ref: '#/components/responses/NotFound'},
          '502': {$ref: '#/components/responses/Unavailable'},
        },
      },
    },
    '/v1/attestations/verify': {
      post: {
        operationId: 'verifyAttestationSignature',
        summary: 'Verify attestation integrity and optional signer binding',
        description:
          'A valid signature proves authorship by the carried signer. It does not establish that the ' +
          'signer is trusted unless expected_signer is supplied from an independent trust source.',
        tags: ['Attestations'],
        requestBody: {
          required: true,
          content: {
            'application/json': {
              schema: {
                type: 'object',
                required: ['attestation'],
                properties: {
                  attestation: {
                    oneOf: [{type: 'object'}, {type: 'string'}],
                  },
                  expected_signer: {
                    type: 'string',
                    description: 'Expected base58 Ed25519 public key',
                  },
                },
              },
            },
          },
        },
        responses: {
          '200': {
            description: 'Cryptographic verification result',
            content: {
              'application/json': {
                schema: {$ref: '#/components/schemas/SignatureVerdict'},
              },
            },
          },
          '400': {$ref: '#/components/responses/BadRequest'},
        },
      },
    },
  },
  components: {
    responses: {
      BadRequest: {
        description: 'Invalid request',
        content: {
          'application/json': {
            schema: {$ref: '#/components/schemas/Error'},
          },
        },
      },
      NotFound: {
        description: 'Resource not found',
        content: {
          'application/json': {
            schema: {$ref: '#/components/schemas/Error'},
          },
        },
      },
      Unavailable: {
        description: 'An upstream public data source is unavailable',
        content: {
          'application/json': {
            schema: {$ref: '#/components/schemas/Error'},
          },
        },
      },
    },
    schemas: {
      Error: {
        type: 'object',
        required: ['error'],
        properties: {error: {type: 'string'}},
      },
      Health: {
        type: 'object',
        required: ['ok', 'service', 'version'],
        properties: {
          ok: {type: 'boolean', const: true},
          service: {type: 'string', const: 'covenant-trust'},
          version: {type: 'string'},
        },
      },
      PaymentHistory: {
        type: 'object',
        required: [
          'wallet',
          'observed_at',
          'observed_inbound_transfers',
          'distinct_senders',
          'volume_micro_usdc',
          'observations',
          'source_fee_payer',
          'classification',
          'settlement_receipt_linked',
          'coverage',
        ],
        properties: {
          wallet: {type: 'string'},
          observed_at: {type: 'string', format: 'date-time'},
          observed_inbound_transfers: {type: 'integer', minimum: 0},
          distinct_senders: {type: 'integer', minimum: 0},
          volume_micro_usdc: {
            type: 'string',
            pattern: '^[0-9]+$',
            description: 'Exact integer micro-USDC amount',
          },
          observations: {
            type: 'array',
            description: 'On-chain references that reproduce the reported aggregate',
            items: {
              type: 'object',
              required: [
                'transaction_signature',
                'slot',
                'block_time',
                'sender',
                'amount_micro_usdc',
                'mint',
              ],
              properties: {
                transaction_signature: {type: 'string'},
                slot: {type: 'integer', minimum: 0},
                block_time: {type: ['integer', 'null']},
                sender: {type: 'string'},
                amount_micro_usdc: {type: 'string', pattern: '^[0-9]+$'},
                mint: {type: 'string'},
              },
            },
          },
          source_fee_payer: {type: 'string'},
          classification: {type: 'string', const: 'payai-sponsored-usdc-transfer'},
          settlement_receipt_linked: {type: 'boolean', const: false},
          coverage: {
            type: 'object',
            required: [
              'signatures_requested',
              'signatures_returned',
              'signatures_candidates',
              'signatures_scanned',
              'signatures_unavailable',
              'oldest_slot',
              'newest_slot',
            ],
            properties: {
              signatures_requested: {type: 'integer', minimum: 1, maximum: 1000},
              signatures_returned: {type: 'integer', minimum: 0},
              signatures_candidates: {type: 'integer', minimum: 0},
              signatures_scanned: {type: 'integer', minimum: 0},
              signatures_unavailable: {type: 'integer', minimum: 0},
              oldest_slot: {type: ['integer', 'null']},
              newest_slot: {type: ['integer', 'null']},
            },
          },
        },
      },
      ValidationVerdict: {
        type: 'object',
        required: [
          'asset',
          'recordAuthentic',
          'evidenceVerified',
          'policyAccepted',
          'subjectRegistrationVerified',
          'profile',
          'legacy',
          'subjectAsset',
          'authority',
          'responseHash',
          'recordedAt',
          'reasons',
        ],
        properties: {
          asset: {type: ['string', 'null']},
          recordAuthentic: {type: 'boolean'},
          evidenceVerified: {
            type: 'null',
            description: 'Not evaluated by this endpoint',
          },
          policyAccepted: {
            type: 'null',
            description: 'The caller must apply its own policy',
          },
          subjectRegistrationVerified: {
            type: 'null',
            description:
              'Full MIP-014 account discriminator and stored-subject verification is not performed',
          },
          profile: {type: 'string'},
          legacy: {type: 'boolean'},
          subjectAsset: {type: ['string', 'null']},
          authority: {type: ['string', 'null']},
          responseHash: {type: ['string', 'null']},
          recordedAt: {type: ['integer', 'null']},
          reasons: {type: 'array', items: {type: 'string'}},
        },
      },
      AgentPassport: {
        type: 'object',
        required: [
          'asset',
          'registry',
          'attestation',
          'validationRecords',
          'legacyAttestation',
        ],
        properties: {
          asset: {type: 'object', additionalProperties: true},
          registry: {
            type: 'object',
            required: [
              'pda',
              'accountOwnerMatches',
              'identityPluginIndexed',
              'registrationUri',
            ],
            properties: {
              pda: {type: 'string'},
              accountOwnerMatches: {
                type: ['boolean', 'null'],
                description:
                  'Owner-only PDA check; not a decoded MIP-014 registration verdict',
              },
              identityPluginIndexed: {
                type: 'boolean',
                description: 'AgentIdentity plugin presence as reported by DAS',
              },
              registrationUri: {
                type: ['string', 'null'],
                description: 'Untrusted on-chain data; not fetched by Covenant Trust',
              },
            },
          },
          attestation: {
            anyOf: [
              {$ref: '#/components/schemas/ValidationVerdict'},
              {type: 'null'},
            ],
          },
          validationRecords: {
            anyOf: [
              {
                type: 'object',
                required: ['count', 'latestObserved', 'coverage'],
                properties: {
                  count: {type: 'integer', minimum: 0},
                  latestObserved: {
                    anyOf: [
                      {$ref: '#/components/schemas/ValidationVerdict'},
                      {type: 'null'},
                    ],
                  },
                  coverage: {
                    type: 'object',
                    required: [
                      'method',
                      'owner',
                      'pagesScanned',
                      'assetsScanned',
                      'truncated',
                      'complete',
                    ],
                    properties: {
                      method: {type: 'string', const: 'validator-owned-assets'},
                      owner: {type: 'string'},
                      pagesScanned: {type: 'integer', minimum: 0},
                      assetsScanned: {type: 'integer', minimum: 0},
                      truncated: {type: 'boolean'},
                      complete: {
                        type: 'boolean',
                        const: false,
                        description:
                          'Ownership indexing cannot prove complete validator-authored history',
                      },
                    },
                  },
                },
              },
              {type: 'null'},
            ],
          },
          legacyAttestation: {
            anyOf: [
              {$ref: '#/components/schemas/ValidationVerdict'},
              {type: 'null'},
            ],
          },
        },
      },
      SignatureVerdict: {
        oneOf: [
          {
            type: 'object',
            required: ['ok', 'subject', 'signer', 'signatureValid', 'signerMatches'],
            properties: {
              ok: {type: 'boolean', const: true},
              subject: {type: 'string'},
              signer: {type: 'string'},
              signatureValid: {type: 'boolean', const: true},
              signerMatches: {type: ['boolean', 'null']},
            },
          },
          {
            type: 'object',
            required: ['ok', 'reason'],
            properties: {
              ok: {type: 'boolean', const: false},
              reason: {type: 'string'},
            },
          },
        ],
      },
    },
  },
} as const;
