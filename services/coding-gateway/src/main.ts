import { ConfigError } from './config-error.js';

// Configuration is validated while ./server.js is evaluated, so a rejected env
// value arrives here as an import rejection: print the message and exit 1.
// Everything else keeps its stack trace, because everything else is a bug.
try {
  await import('./server.js');
} catch (cause) {
  if (!(cause instanceof ConfigError)) throw cause;
  console.error(cause.message);
  process.exit(1);
}
