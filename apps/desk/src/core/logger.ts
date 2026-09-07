/**
 * Structured logging.
 *
 * One JSON object per line on stderr, so stdout stays clean for the MCP stdio
 * transport and for CLI output. Every line passes through {@link redact} with
 * the keystore's values registered, so a key cannot reach a log file.
 */

import { redact } from './keystore.js';

export type LogLevel = 'debug' | 'info' | 'warn' | 'error';

const LEVEL_ORDER: Record<LogLevel, number> = { debug: 10, info: 20, warn: 30, error: 40 };

export interface Logger {
  debug(message: string, fields?: Record<string, unknown>): void;
  info(message: string, fields?: Record<string, unknown>): void;
  warn(message: string, fields?: Record<string, unknown>): void;
  error(message: string, fields?: Record<string, unknown>): void;
  /** A logger that adds fields to every line. */
  child(fields: Record<string, unknown>): Logger;
  /** Register a value that must never appear in a log line. */
  addSecret(value: string): void;
  readonly level: LogLevel;
}

export interface LoggerOptions {
  level?: LogLevel;
  /** Component name written as `component` on every line. */
  component?: string;
  /** Values masked wherever they appear. */
  secrets?: readonly string[];
  /** Where a formatted line goes. Defaults to stderr. */
  sink?: (line: string) => void;
  /** Clock, for tests. */
  now?: () => number;
}

/** Create a logger. */
export function createLogger(options: LoggerOptions = {}): Logger {
  const secrets = new Set<string>(options.secrets ?? []);
  const sink = options.sink ?? ((line: string) => process.stderr.write(`${line}\n`));
  const now = options.now ?? Date.now;
  const level = options.level ?? 'info';

  const build = (base: Record<string, unknown>, activeLevel: LogLevel): Logger => {
    const write = (lineLevel: LogLevel, message: string, fields?: Record<string, unknown>) => {
      if (LEVEL_ORDER[lineLevel] < LEVEL_ORDER[activeLevel]) return;
      const record = {
        ts: new Date(now()).toISOString(),
        level: lineLevel,
        msg: message,
        ...base,
        ...(fields ?? {}),
      };
      const safe = redact(record, [...secrets]);
      sink(JSON.stringify(safe, jsonReplacer));
    };
    return {
      level: activeLevel,
      debug: (message, fields) => write('debug', message, fields),
      info: (message, fields) => write('info', message, fields),
      warn: (message, fields) => write('warn', message, fields),
      error: (message, fields) => write('error', message, fields),
      child: (fields) => build({ ...base, ...fields }, activeLevel),
      addSecret: (value) => {
        if (value && value.length >= 8) secrets.add(value);
      },
    };
  };

  return build(options.component ? { component: options.component } : {}, level);
}

function jsonReplacer(_key: string, value: unknown): unknown {
  return typeof value === 'bigint' ? value.toString() : value;
}

/** A logger that drops every line. Useful in tests. */
export function silentLogger(): Logger {
  return createLogger({ level: 'error', sink: () => {} });
}
