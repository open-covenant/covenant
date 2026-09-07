/**
 * Covenant Desk: a local execution desk for Robinhood Chain stock tokens and
 * the tokens paired with them.
 *
 * Keys stay on the machine that runs it. Orders are dry runs until live
 * execution is turned on in the config file and asked for on the order.
 */

export * from './core/index.js';
export * from './chain/index.js';
export * from './fairvalue/index.js';
export * from './orders/index.js';
export * from './hedge/index.js';
export { createDesk, runDaemon, type Desk, type CreateDeskOptions } from './daemon.js';
export { createHttpSurface, ROUTES, type HttpSurface } from './surfaces/http/index.js';
export { createMcpSurface, TOOL_NAMES, TOOL_SUMMARIES, type McpSurface } from './surfaces/mcp/index.js';
export { createUiSurface, type UiSurface } from './surfaces/ui/index.js';
export { runCli, helpText, version, COMMANDS } from './surfaces/cli/index.js';
