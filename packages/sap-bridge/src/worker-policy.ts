export type WorkerSignerRole = 'payer' | 'verifier' | null;

export function signerRoleForCommand(command: string): WorkerSignerRole {
  switch (command) {
    case 'publish-agent':
    case 'update-agent':
    case 'attest-root':
      return 'payer';
    case 'attest-agent':
      return 'verifier';
    default:
      return null;
  }
}
