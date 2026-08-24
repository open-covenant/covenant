const SUPPORTED_E2B_EGRESS_HOSTS = new Set([
  'github.com',
  'codeload.github.com',
  'objects.githubusercontent.com',
  'registry.npmjs.org',
  'registry.yarnpkg.com',
  'pypi.org',
  'files.pythonhosted.org',
  'crates.io',
  'static.crates.io',
]);

const HOSTNAME =
  /^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?$/;

export function parseE2bEgressPolicy(raw: string | undefined): readonly string[] {
  if (raw === undefined || raw.trim() === '') return Object.freeze([]);

  const hosts = raw.split(',').map((value) => value.trim().toLowerCase());
  if (hosts.some((host) => !host)) {
    throw new Error('E2B_EGRESS_ALLOW must not contain empty entries');
  }

  return validatePolicy(hosts, 'E2B_EGRESS_ALLOW');
}

export function validateE2bEgressPolicy(hosts: readonly string[]): readonly string[] {
  return validatePolicy(hosts, 'E2B egress policy');
}

export function validateRunEgressAllowlist(hosts: readonly string[]): readonly string[] {
  return validateHostList(hosts, 'sandbox egress allowlist');
}

function validatePolicy(hosts: readonly string[], name: string): readonly string[] {
  const validated = validateHostList(hosts, name);
  for (const host of validated) {
    if (!SUPPORTED_E2B_EGRESS_HOSTS.has(host)) {
      throw new Error(`${name} contains unsupported host ${host}`);
    }
  }
  return validated;
}

function validateHostList(hosts: readonly string[], name: string): readonly string[] {
  const unique = new Set<string>();
  for (const host of hosts) {
    assertHostname(host, name);
    if (unique.has(host)) {
      throw new Error(`${name} contains duplicate host ${host}`);
    }
    unique.add(host);
  }

  return Object.freeze([...unique]);
}

function assertHostname(host: string, name: string): void {
  if (host !== host.trim() || host !== host.toLowerCase() || !HOSTNAME.test(host)) {
    throw new Error(`${name} contains invalid hostname ${JSON.stringify(host)}`);
  }
}
