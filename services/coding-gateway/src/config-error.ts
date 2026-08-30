/**
 * A rejected environment value. The entry point prints the message alone and
 * exits 1, so an operator fixing a typo sees the accepted range rather than a
 * Node stack trace.
 */
export class ConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ConfigError';
  }
}
