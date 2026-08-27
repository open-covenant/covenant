// Generated projection of agent-os/target/idl/settlement.json.
// This describes staged source code, not a currently supported mainnet deployment.
export const computeSettlementIdl = {
  address: 'cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y',
  instructions: [
    {
      name: 'initialize_compute_payments',
      discriminator: [203, 23, 106, 165, 56, 40, 73, 54],
      accounts: [
        { name: 'config' },
        { name: 'compute_config' },
        { name: 'authority' },
        { name: 'usdc_mint' },
        { name: 'system_program' },
      ],
      args: [{ name: 'settlement_authority', type: 'pubkey' }],
    },
    {
      name: 'update_compute_settlement_authority',
      discriminator: [185, 194, 121, 40, 171, 188, 232, 133],
      accounts: [{ name: 'config' }, { name: 'compute_config' }, { name: 'authority' }],
      args: [{ name: 'settlement_authority', type: 'pubkey' }],
    },
    {
      name: 'fund_compute_job',
      discriminator: [81, 192, 96, 87, 230, 189, 16, 123],
      accounts: [
        { name: 'config' },
        { name: 'compute_config' },
        { name: 'escrow' },
        { name: 'client' },
        { name: 'client_usdc' },
        { name: 'provider_usdc' },
        { name: 'escrow_vault' },
        { name: 'usdc_mint' },
        { name: 'token_program' },
        { name: 'system_program' },
      ],
      args: [{ name: 'args', type: { defined: { name: 'FundComputeJobArgs' } } }],
    },
    {
      name: 'settle_compute_job',
      discriminator: [241, 47, 176, 226, 226, 179, 45, 144],
      accounts: [
        { name: 'config' },
        { name: 'compute_config' },
        { name: 'escrow' },
        { name: 'settlement_authority' },
        { name: 'escrow_vault' },
        { name: 'provider_usdc' },
        { name: 'client_usdc' },
        { name: 'usdc_mint' },
        { name: 'token_program' },
      ],
      args: [
        { name: 'actual_usdc_amount', type: 'u64' },
        { name: 'receipt_commitment', type: { array: ['u8', 32] } },
      ],
    },
    {
      name: 'refund_compute_job',
      discriminator: [210, 10, 125, 130, 234, 164, 11, 181],
      accounts: [
        { name: 'config' },
        { name: 'compute_config' },
        { name: 'escrow' },
        { name: 'authority' },
        { name: 'escrow_vault' },
        { name: 'client_usdc' },
        { name: 'usdc_mint' },
        { name: 'token_program' },
      ],
      args: [{ name: 'refund_commitment', type: { array: ['u8', 32] } }],
    },
  ],
  accounts: [
    {
      name: 'ComputeEscrow',
      discriminator: [56, 57, 151, 207, 152, 81, 212, 113],
    },
    {
      name: 'ComputePaymentConfig',
      discriminator: [199, 106, 161, 139, 149, 124, 183, 244],
    },
  ],
  types: [
    {
      name: 'ComputeEscrow',
      type: {
        kind: 'struct',
        fields: [
          { name: 'job_id', type: { array: ['u8', 32] } },
          { name: 'quote_commitment', type: { array: ['u8', 32] } },
          { name: 'client', type: 'pubkey' },
          { name: 'provider', type: 'pubkey' },
          { name: 'client_usdc', type: 'pubkey' },
          { name: 'provider_usdc', type: 'pubkey' },
          { name: 'escrow_vault', type: 'pubkey' },
          { name: 'usdc_mint', type: 'pubkey' },
          { name: 'max_usdc_amount', type: 'u64' },
          { name: 'actual_usdc_amount', type: 'u64' },
          { name: 'refunded_usdc_amount', type: 'u64' },
          { name: 'expires_at', type: 'i64' },
          { name: 'created_at', type: 'i64' },
          { name: 'terminal_at', type: 'i64' },
          { name: 'terminal_commitment', type: { array: ['u8', 32] } },
          { name: 'terminal_authority', type: 'pubkey' },
          { name: 'status', type: 'u8' },
          { name: 'bump', type: 'u8' },
        ],
      },
    },
    {
      name: 'ComputePaymentConfig',
      type: {
        kind: 'struct',
        fields: [
          { name: 'usdc_mint', type: 'pubkey' },
          { name: 'settlement_authority', type: 'pubkey' },
          { name: 'bump', type: 'u8' },
        ],
      },
    },
    {
      name: 'FundComputeJobArgs',
      type: {
        kind: 'struct',
        fields: [
          { name: 'job_id', type: { array: ['u8', 32] } },
          { name: 'quote_commitment', type: { array: ['u8', 32] } },
          { name: 'provider', type: 'pubkey' },
          { name: 'max_usdc_amount', type: 'u64' },
          { name: 'expires_at', type: 'i64' },
        ],
      },
    },
  ],
} as const;
