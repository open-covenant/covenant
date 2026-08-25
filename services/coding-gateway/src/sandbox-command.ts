const CACHE_ROOT = '/tmp/mizuki-cache';
const SANDBOX_HOME = '/tmp/mizuki-home';

export function isolatedShellCommand(command: string): string {
  const quoted = quoteShellArgument(command);
  return [
    `mkdir -p ${quoteShellArgument(SANDBOX_HOME)} ${quoteShellArgument(`${CACHE_ROOT}/npm`)} ${quoteShellArgument(`${CACHE_ROOT}/corepack`)} ${quoteShellArgument(`${CACHE_ROOT}/pnpm`)} ${quoteShellArgument(`${CACHE_ROOT}/yarn`)}`,
    [
      `HOME=${quoteShellArgument(SANDBOX_HOME)}`,
      `XDG_CACHE_HOME=${quoteShellArgument(CACHE_ROOT)}`,
      `NPM_CONFIG_CACHE=${quoteShellArgument(`${CACHE_ROOT}/npm`)}`,
      `COREPACK_HOME=${quoteShellArgument(`${CACHE_ROOT}/corepack`)}`,
      `PNPM_HOME=${quoteShellArgument(`${CACHE_ROOT}/pnpm`)}`,
      `YARN_CACHE_FOLDER=${quoteShellArgument(`${CACHE_ROOT}/yarn`)}`,
      `/bin/bash -c ${quoted}`,
    ].join(' '),
  ].join(' && ');
}

export function quoteShellArgument(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}
