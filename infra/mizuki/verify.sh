#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$root"

ruby <<'RUBY'
require 'yaml'
require 'uri'
require 'json'

def load_yaml(path)
  source = File.read(path)
  keyword_api = YAML.method(:safe_load).parameters.any? do |kind, _name|
    %i[key keyreq keyrest].include?(kind)
  end
  return YAML.safe_load(source, permitted_classes: [], permitted_symbols: [], aliases: false) if keyword_api

  YAML.safe_load(source, [], [], false)
end

blueprint = load_yaml('infra/mizuki/render.yaml')
load_yaml('.github/workflows/mizuki.yml')
load_yaml('.github/workflows/mizuki-image.yml')
services = blueprint.fetch('services')
databases = blueprint.fetch('databases')
expected = {
  'mizuki-runtime-shadow' => ['pserv', 'image', nil],
  'mizuki-runtime-production' => ['web', 'image', '/deployz'],
  'mizuki-policy-signer' => ['pserv', 'node', nil],
  'mizuki-coding-gateway' => ['pserv', 'node', nil],
  'mizuki' => ['web', 'node', '/healthz'],
  'mizuki-deployment-controller' => ['pserv', 'node', nil],
  'mizuki-updater' => ['pserv', 'node', nil]
}
database_names = %w[
  mizuki-postgres
  mizuki-signer-postgres
  mizuki-updater-postgres
  mizuki-deployment-postgres
  mizuki-runtime-shadow-postgres
]
image_repository = 'ghcr.io/open-covenant/covenant/mizuki'
image_reference = /\A#{Regexp.escape(image_repository)}@sha256:[0-9a-f]{64}\z/

abort 'service set mismatch' unless services.map { |service| service.fetch('name') }.sort == expected.keys.sort
abort 'database set mismatch' unless databases.map { |database| database.fetch('name') }.sort == database_names.sort

services.each do |service|
  type, runtime, health = expected.fetch(service.fetch('name'))
  abort "wrong service type: #{service['name']}" unless service['type'] == type
  abort "wrong runtime: #{service['name']}" unless service['runtime'] == runtime
  abort "non-durable service plan: #{service['name']}" if service['plan'] == 'free'
  abort "service region drift: #{service['name']}" unless service['region'] == 'frankfurt'
  abort "service must be single-instance: #{service['name']}" unless service['numInstances'] == 1
  abort "automatic deploy enabled: #{service['name']}" unless service['autoDeploy'] == false
  abort "health check mismatch: #{service['name']}" if health && service['healthCheckPath'] != health
  abort "private service has a public health check: #{service['name']}" if !health && service.key?('healthCheckPath')

  if runtime == 'node'
    abort "service branch is not protected main: #{service['name']}" unless service['branch'] == 'main'
    abort "Git-backed service has no repository: #{service['name']}" unless service['repo'] == 'https://github.com/open-covenant/covenant'
  else
    abort "image service has a Git source: #{service['name']}" if service.key?('repo') || service.key?('branch')
    abort "image service has a build command: #{service['name']}" if service.key?('buildCommand') || service.key?('startCommand')
    image = service.fetch('image')
    abort "image service is not digest-pinned: #{service['name']}" unless image.fetch('url').match?(image_reference)
    credential = image.dig('creds', 'fromRegistryCreds', 'name')
    abort "image service registry credential drift: #{service['name']}" unless credential == 'mizuki-ghcr'
    abort "image service predeploy gate missing: #{service['name']}" unless service['preDeployCommand'] == 'node dist/predeploy-cli.js'
  end

  keys = service.fetch('envVars').map { |item| item.fetch('key') }
  abort "duplicate environment key: #{service['name']}" unless keys.uniq.size == keys.size
  service.fetch('envVars').each do |item|
    sources = %w[value sync generateValue fromDatabase fromService].count { |key| item.key?(key) }
    abort "invalid environment source: #{service['name']}/#{item['key']}" unless sources == 1
    next unless item['fromDatabase']

    name = item['fromDatabase']['name']
    abort "unknown database reference: #{service['name']}/#{item['key']}" unless database_names.include?(name)
  end
end

databases.each do |database|
  abort "non-durable database plan: #{database['name']}" if database['plan'] == 'free'
  abort "database disk too small: #{database['name']}" unless database['diskSizeGB'].to_i >= 5
  abort "database autoscaling disabled: #{database['name']}" unless database['storageAutoscalingEnabled'] == true
  abort "database exposes an IP allowlist: #{database['name']}" unless database['ipAllowList'] == []
  abort "database must use direct connections: #{database['name']}" unless database['connectionPool'] == 'none'
  abort "database region drift: #{database['name']}" unless database['region'] == 'frankfurt'
  abort "database major version drift: #{database['name']}" unless database['postgresMajorVersion'] == '16'
end

env = services.to_h do |service|
  [service.fetch('name'), service.fetch('envVars').to_h { |item| [item.fetch('key'), item] }]
end

shadow = env.fetch('mizuki-runtime-shadow')
production = env.fetch('mizuki-runtime-production')
signer = env.fetch('mizuki-policy-signer')
gateway = env.fetch('mizuki-coding-gateway')
web = env.fetch('mizuki')
updater = env.fetch('mizuki-updater')
controller = env.fetch('mizuki-deployment-controller')

shadow_service = services.find { |service| service['name'] == 'mizuki-runtime-shadow' }
production_service = services.find { |service| service['name'] == 'mizuki-runtime-production' }
abort 'runtime baseline image mismatch' unless shadow_service.dig('image', 'url') == production_service.dig('image', 'url')

def service_ref(item, name, key = nil)
  ref = item.fetch('fromService')
  ref['name'] == name && (key.nil? || ref['envVarKey'] == key)
end

abort 'web proxy secret is not linked to the canonical production runtime' unless service_ref(web.fetch('MIZUKI_WEB_PROXY_SECRET'), 'mizuki-runtime-production', 'MIZUKI_WEB_PROXY_SECRET')
abort 'web API origin is not linked to the canonical production runtime' unless service_ref(web.fetch('MIZUKI_API_URL'), 'mizuki-runtime-production', 'RENDER_EXTERNAL_URL')
abort 'web Solana RPC is not operator-pinned' unless web.fetch('NEXT_PUBLIC_SOLANA_RPC_URL')['sync'] == false

runtime_services = [shadow, production]
runtime_services.each do |runtime|
  abort 'runtime Render proxy trust drift' unless runtime.fetch('MIZUKI_TRUSTED_PROXY_HOPS')['value'] == '1'
  abort 'runtime rate-limit source capacity drift' unless runtime.fetch('MIZUKI_RATE_LIMIT_MAX_SOURCES')['value'] == '10000'
  abort 'runtime SSE global cap drift' unless runtime.fetch('MIZUKI_SSE_MAX_CONNECTIONS')['value'] == '100'
  abort 'runtime SSE source cap drift' unless runtime.fetch('MIZUKI_SSE_MAX_CONNECTIONS_PER_SOURCE')['value'] == '3'
  abort 'runtime SSE idle timeout drift' unless runtime.fetch('MIZUKI_SSE_IDLE_TIMEOUT_MS')['value'] == '120000'
  abort 'runtime readiness refresh drift' unless runtime.fetch('MIZUKI_READINESS_REFRESH_MS')['value'] == '30000'
  abort 'runtime readiness max age drift' unless runtime.fetch('MIZUKI_READINESS_MAX_AGE_MS')['value'] == '90000'
  abort 'runtime readiness timeout drift' unless runtime.fetch('MIZUKI_READINESS_TIMEOUT_MS')['value'] == '20000'
  abort 'runtime escrow readiness floor drift' unless runtime.fetch('MIZUKI_ESCROW_READINESS_MIN_LAMPORTS')['value'] == '1000000000'
  abort 'runtime signer URL is not private service discovery' unless service_ref(runtime.fetch('MIZUKI_POLICY_SIGNER_URL'), 'mizuki-policy-signer')
  abort 'runtime signer token is not linked to signer' unless service_ref(runtime.fetch('MIZUKI_POLICY_SIGNER_TOKEN'), 'mizuki-policy-signer', 'MIZUKI_SIGNER_AUTH_TOKEN')
  abort 'runtime gateway URL is not private service discovery' unless service_ref(runtime.fetch('MIZUKI_CODING_GATEWAY_URL'), 'mizuki-coding-gateway')
  abort 'runtime gateway token is not linked to gateway' unless service_ref(runtime.fetch('MIZUKI_CODING_GATEWAY_TOKEN'), 'mizuki-coding-gateway', 'CODER_AUTH_TOKEN')
  abort 'runtime updater URL is not private service discovery' unless service_ref(runtime.fetch('MIZUKI_UPDATER_URL'), 'mizuki-updater')
  abort 'runtime updater token is not read-only' unless service_ref(runtime.fetch('MIZUKI_UPDATER_TOKEN'), 'mizuki-updater', 'MIZUKI_UPDATER_READ_TOKEN')
  abort 'runtime payment recipient is not the signer refund treasury' unless service_ref(runtime.fetch('MIZUKI_PAY_TO'), 'mizuki-policy-signer', 'MIZUKI_REFUND_TREASURY')
  abort 'runtime escrow refund destination is not the isolated escrow authority' unless service_ref(runtime.fetch('MIZUKI_ESCROW_REFUND_TO'), 'mizuki-policy-signer', 'MIZUKI_ESCROW_AUTHORITY')
  abort 'runtime x402 facilitator is not pinned to HTTPS' unless URI(runtime.fetch('MIZUKI_X402_FACILITATOR')['value']).scheme == 'https'
  abort 'runtime UsePod origin drift' unless runtime.fetch('USEPOD_BASE_URL')['value'] == 'https://api.usepod.ai'
  abort 'runtime UsePod input ceiling drift' unless runtime.fetch('USEPOD_MAX_INPUT_PRICE_MICROUNITS')['value'] == '200000'
  abort 'runtime UsePod output ceiling drift' unless runtime.fetch('USEPOD_MAX_OUTPUT_PRICE_MICROUNITS')['value'] == '400000'
  abort 'runtime payment mode is not live' unless runtime.fetch('MIZUKI_PAYMENT_MODE')['value'] == 'live'
  abort 'runtime GitHub App requirement disabled' unless runtime.fetch('MIZUKI_REQUIRE_GITHUB_APP')['value'] == '1'
  abort 'runtime functional probe token is not linked to controller' unless service_ref(runtime.fetch('MIZUKI_RELEASE_PROBE_TOKEN'), 'mizuki-deployment-controller', 'MIZUKI_DEPLOY_PROBE_TOKEN')
end

abort 'production runtime is not bound to the canonical commercial database' unless production.fetch('MIZUKI_DATABASE_URL').dig('fromDatabase', 'name') == 'mizuki-postgres'
abort 'shadow runtime database not isolated' unless shadow.fetch('MIZUKI_DATABASE_URL').dig('fromDatabase', 'name') == 'mizuki-runtime-shadow-postgres'
abort 'runtime databases are shared' if production.fetch('MIZUKI_DATABASE_URL').dig('fromDatabase', 'name') == shadow.fetch('MIZUKI_DATABASE_URL').dig('fromDatabase', 'name')
abort 'production public origin does not follow its Render URL' unless service_ref(production.fetch('MIZUKI_PUBLIC_BASE_URL'), 'mizuki-runtime-production', 'RENDER_EXTERNAL_URL')
abort 'shadow public origin does not follow production' unless service_ref(shadow.fetch('MIZUKI_PUBLIC_BASE_URL'), 'mizuki-runtime-production', 'MIZUKI_PUBLIC_BASE_URL')
abort 'production web proxy secret is not generated' unless production.fetch('MIZUKI_WEB_PROXY_SECRET')['generateValue'] == true
abort 'production ClawPump payout is not linked to the escrow authority' unless service_ref(production.fetch('CLAWPUMP_PAYOUT_WALLET'), 'mizuki-policy-signer', 'MIZUKI_ESCROW_AUTHORITY')
abort 'production authority seed is not operator-pinned' unless production.fetch('MIZUKI_JOB_AUTHORITY_SEED')['sync'] == false
abort 'shadow authority seed does not follow production' unless service_ref(shadow.fetch('MIZUKI_JOB_AUTHORITY_SEED'), 'mizuki-runtime-production', 'MIZUKI_JOB_AUTHORITY_SEED')
%w[
  USEPOD_API_KEY
  USEPOD_MODEL
  USEPOD_REVIEW_MODEL
  USEPOD_MIN_BALANCE
  MIZUKI_GITHUB_APP_ID
  MIZUKI_GITHUB_PRIVATE_KEY
  MIZUKI_GITHUB_CLIENT_ID
  MIZUKI_GITHUB_CLIENT_SECRET
  MIZUKI_GITHUB_WEBHOOK_SECRET
].each do |key|
  abort "production runtime secret is not operator-pinned: #{key}" unless production.fetch(key)['sync'] == false
  abort "shadow runtime secret does not follow production: #{key}" unless service_ref(shadow.fetch(key), 'mizuki-runtime-production', key)
end

abort 'signer auth token is not generated' unless signer.fetch('MIZUKI_SIGNER_AUTH_TOKEN')['generateValue'] == true
abort 'refund key is not secret' unless signer.fetch('MIZUKI_REFUND_PRIVATE_KEY_JSON')['sync'] == false
abort 'escrow key is not secret' unless signer.fetch('MIZUKI_ESCROW_PRIVATE_KEY_JSON')['sync'] == false
abort 'signer verifier App ID is not operator-pinned' unless signer.fetch('MIZUKI_SIGNER_GITHUB_APP_ID')['sync'] == false
abort 'signer verifier App key is not secret' unless signer.fetch('MIZUKI_SIGNER_GITHUB_PRIVATE_KEY')['sync'] == false
abort 'stale signer repository credential setting present' if signer.key?('MIZUKI_SIGNER_GITHUB_TOKEN')
abort 'escrow authority is not operator-pinned' unless signer.fetch('MIZUKI_ESCROW_AUTHORITY')['sync'] == false
abort 'job authority public key is not operator-pinned' unless signer.fetch('MIZUKI_JOB_AUTHORITY_PUBLIC_KEY')['sync'] == false
abort 'primary RPC is not operator-pinned' unless signer.fetch('MIZUKI_SIGNER_RPC_URL')['sync'] == false
abort 'secondary RPC is not operator-pinned' unless signer.fetch('MIZUKI_SIGNER_SECONDARY_RPC_URL')['sync'] == false
abort 'signer RPC timeout drift' unless signer.fetch('MIZUKI_SIGNER_RPC_TIMEOUT_MS')['value'] == '5000'
abort 'primary price URL drift' unless signer.fetch('MIZUKI_SOL_USD_PRICE_URL')['value'] == 'https://api.exchange.coinbase.com/products/SOL-USD/ticker'
abort 'secondary price URL drift' unless signer.fetch('MIZUKI_SOL_USD_SECONDARY_PRICE_URL')['value'] == 'https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd&include_last_updated_at=true&precision=6'
abort 'public price feeds must not require tokens' if signer.key?('MIZUKI_SOL_USD_PRICE_TOKEN') || signer.key?('MIZUKI_SOL_USD_SECONDARY_PRICE_TOKEN')
abort 'price divergence limit drift' unless signer.fetch('MIZUKI_SOL_USD_MAX_DIVERGENCE_BPS')['value'] == '500'
abort 'price observation age drift' unless signer.fetch('MIZUKI_SOL_USD_MAX_AGE_MS')['value'] == '300000'
abort 'signer mock mode enabled' unless signer.fetch('MIZUKI_SIGNER_MOCK_MODE')['value'] == 'false'
abort 'wrong operation limit' unless signer.fetch('MIZUKI_OPERATION_LIMIT_USD_CENTS')['value'] == '2500'
abort 'wrong refund limit' unless signer.fetch('MIZUKI_REFUND_DAILY_LIMIT_USD_CENTS')['value'] == '10000'
abort 'wrong escrow limit' unless signer.fetch('MIZUKI_ESCROW_DAILY_LIMIT_USD_CENTS')['value'] == '10000'
abort 'refund authorization TTL drift' unless signer.fetch('MIZUKI_REFUND_AUTH_MAX_TTL_SECONDS')['value'] == '900'
abort 'liability registration window drift' unless signer.fetch('MIZUKI_REFUND_LIABILITY_MAX_AGE_SECONDS')['value'] == '86400'
abort 'signer database not isolated' unless signer.fetch('MIZUKI_SIGNER_DATABASE_URL').dig('fromDatabase', 'name') == 'mizuki-signer-postgres'
abort 'updater database not isolated' unless updater.fetch('MIZUKI_UPDATER_DATABASE_URL').dig('fromDatabase', 'name') == 'mizuki-updater-postgres'
abort 'deployment controller database not isolated' unless controller.fetch('MIZUKI_DEPLOY_DATABASE_URL').dig('fromDatabase', 'name') == 'mizuki-deployment-postgres'
abort 'deployment controller database transport drift' unless controller.fetch('MIZUKI_DEPLOY_DATABASE_SSL_MODE')['value'] == 'disable'
abort 'deployment controller database timeout drift' unless controller.fetch('MIZUKI_DEPLOY_DATABASE_CONNECT_TIMEOUT_MS')['value'] == '10000'
abort 'deployment controller database pool drift' unless controller.fetch('MIZUKI_DEPLOY_DATABASE_MAX_CONNECTIONS')['value'] == '8'
abort 'deployment image repository drift' unless controller.fetch('MIZUKI_DEPLOY_IMAGE_REPOSITORY')['value'] == image_repository
abort 'deployment artifact origin drift' unless controller.fetch('MIZUKI_DEPLOY_ARTIFACT_ORIGINS')['value'] == 'https://github.com,https://release-assets.githubusercontent.com'
abort 'deployment Render API key is not operator-pinned' unless controller.fetch('MIZUKI_DEPLOY_RENDER_API_KEY')['sync'] == false
abort 'deployment Render API origin drift' unless controller.fetch('MIZUKI_DEPLOY_RENDER_API_URL')['value'] == 'https://api.render.com/v1'
abort 'deployment shadow service ID is not operator-pinned' unless controller.fetch('MIZUKI_DEPLOY_RENDER_SHADOW_SERVICE_ID')['sync'] == false
abort 'deployment production service ID is not operator-pinned' unless controller.fetch('MIZUKI_DEPLOY_RENDER_PRODUCTION_SERVICE_ID')['sync'] == false
abort 'deployment service allowlist is not operator-pinned' unless controller.fetch('MIZUKI_DEPLOY_RENDER_ALLOWED_SERVICE_IDS')['sync'] == false
abort 'deployment shadow probe URL drift' unless controller.fetch('MIZUKI_DEPLOY_SHADOW_PROBE_URL')['value'] == 'http://mizuki-runtime-shadow:10000/internal/mizuki/functional-readiness'
abort 'deployment production probe URL is not operator-pinned' unless controller.fetch('MIZUKI_DEPLOY_PRODUCTION_PROBE_URL')['sync'] == false
abort 'deployment probe token is not generated' unless controller.fetch('MIZUKI_DEPLOY_PROBE_TOKEN')['generateValue'] == true
abort 'deployment Render timeout drift' unless controller.fetch('MIZUKI_DEPLOY_RENDER_TIMEOUT_MS')['value'] == '20000'
abort 'deployment artifact timeout drift' unless controller.fetch('MIZUKI_DEPLOY_ARTIFACT_TIMEOUT_MS')['value'] == '30000'
abort 'deployment probe timeout drift' unless controller.fetch('MIZUKI_DEPLOY_PROBE_TIMEOUT_MS')['value'] == '10000'
abort 'deployment reconciliation grace drift' unless controller.fetch('MIZUKI_DEPLOY_RECONCILIATION_GRACE_MS')['value'] == '120000'
abort 'deployment minimum promotion age drift' unless controller.fetch('MIZUKI_DEPLOY_MIN_PROMOTION_AGE_MS')['value'] == '120000'
abort 'updater read and write tokens are not distinct settings' unless updater.key?('MIZUKI_UPDATER_READ_TOKEN') && updater.key?('MIZUKI_UPDATER_AUTH_TOKEN')
controller_ref = updater.fetch('MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT').fetch('fromService')
abort 'updater controller origin is not private service discovery' unless controller_ref == {
  'type' => 'pserv',
  'name' => 'mizuki-deployment-controller',
  'property' => 'hostport'
}
abort 'updater controller token is not linked' unless service_ref(updater.fetch('MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN'), 'mizuki-deployment-controller', 'MIZUKI_DEPLOY_AUTH_TOKEN')
abort 'updater hook timeout drift' unless updater.fetch('MIZUKI_UPDATER_HOOK_TIMEOUT_MS')['value'] == '90000'
abort 'updater retry horizon drift' unless updater.fetch('MIZUKI_UPDATER_MAX_ATTEMPTS')['value'] == '10'
abort 'updater promotion soak drift' unless updater.fetch('MIZUKI_UPDATER_PROMOTION_SOAK_MS')['value'] == '120000'
abort 'updater promotion timeout drift' unless updater.fetch('MIZUKI_UPDATER_PROMOTION_TIMEOUT_MS')['value'] == '600000'
abort 'updater base branch drift' unless updater.fetch('MIZUKI_UPDATER_ALLOWED_BASE_BRANCHES')['value'] == 'main'
abort 'updater artifact origin drift' unless updater.fetch('MIZUKI_UPDATER_ARTIFACT_ORIGINS')['value'] == 'https://github.com,https://release-assets.githubusercontent.com'
abort 'updater mandatory checks drift' unless updater.fetch('MIZUKI_UPDATER_MANDATORY_CHECKS')['value'] == 'application,escrow,rust,landing'

disk = services.find { |service| service['name'] == 'mizuki-coding-gateway' }.fetch('disk')
abort 'gateway disk is not mounted at /var/data' unless disk['mountPath'] == '/var/data' && disk['sizeGB'].to_i >= 1
abort 'gateway is not pinned to UsePod' unless gateway.fetch('CODER_BACKEND')['value'] == 'usepod'
abort 'gateway has an ambiguous legacy route setting' if gateway.key?('USEPOD_MODEL')
abort 'gateway auth token is not generated' unless gateway.fetch('CODER_AUTH_TOKEN')['generateValue'] == true
abort 'gateway spend ledger is not persistent' unless gateway.fetch('LEDGER_PATH')['value'].start_with?('/var/data/')
abort 'gateway run store is not persistent' unless gateway.fetch('RUN_STORE_PATH')['value'].start_with?('/var/data/')
abort 'gateway readiness refresh drift' unless gateway.fetch('CODER_READINESS_REFRESH_MS')['value'] == '120000'
abort 'gateway readiness max age drift' unless gateway.fetch('CODER_READINESS_MAX_AGE_MS')['value'] == '300000'
abort 'gateway readiness timeout drift' unless gateway.fetch('CODER_READINESS_TIMEOUT_MS')['value'] == '20000'
abort 'gateway sandbox-rate estimate drift' unless gateway.fetch('CODER_SANDBOX_USD_PER_SEC')['value'] == '0.0001'
abort 'gateway UsePod origin drift' unless gateway.fetch('USEPOD_BASE_URL')['value'] == 'https://api.usepod.ai'
abort 'gateway UsePod input ceiling drift' unless gateway.fetch('USEPOD_MAX_INPUT_PRICE_MICROUNITS')['value'] == '200000'
abort 'gateway UsePod output ceiling drift' unless gateway.fetch('USEPOD_MAX_OUTPUT_PRICE_MICROUNITS')['value'] == '400000'
abort 'gateway E2B template drift' unless gateway.fetch('E2B_TEMPLATE')['value'] == 'covenant-coder'

core_app = JSON.parse(File.read('infra/mizuki/github-apps/core.manifest.json'))
verifier_app = JSON.parse(File.read('infra/mizuki/github-apps/policy-verifier.manifest.json'))
updater_app = JSON.parse(File.read('infra/mizuki/github-apps/updater.manifest.json'))
abort 'core App must be public' unless core_app['public'] == true
abort 'core App callback drift' unless core_app['callback_urls'] == ['https://mizuki.covenant.org/api/mizuki/v1/auth/github/callback']
abort 'core App webhook drift' unless core_app.dig('hook_attributes', 'url') == 'https://mizuki.covenant.org/api/mizuki/v1/github/webhook'
abort 'core App permission drift' unless core_app['default_permissions'] == {
  'checks' => 'read',
  'contents' => 'write',
  'issues' => 'read',
  'members' => 'read',
  'metadata' => 'read',
  'pull_requests' => 'write'
}
abort 'policy verifier App must be public' unless verifier_app['public'] == true
abort 'policy verifier App permission drift' unless verifier_app['default_permissions'] == {
  'contents' => 'read',
  'issues' => 'read',
  'metadata' => 'read',
  'pull_requests' => 'read'
}
abort 'policy verifier App must not subscribe to events' unless verifier_app['default_events'] == []
abort 'updater App must remain private' unless updater_app['public'] == false

puts 'Blueprint invariants OK'
RUBY

jq -e '
  .mcpServers["clawpump-agents"].command == "npx" and
  .mcpServers["clawpump-agents"].args == ["@clawpump/agents"] and
  (.mcpServers["clawpump-agents"].env.CLAWPUMP_API_KEY | startswith("cpk_replace"))
' services/mizuki/clawpump/mcp.json.example >/dev/null

scope='.github/workflows/mizuki.yml .github/workflows/mizuki-image.yml apps/mizuki-web services/mizuki services/mizuki-policy-signer services/mizuki-updater services/mizuki-deployment-controller services/coding-gateway infra/mizuki programs/mizuki-escrow docs/production-audit-mizuki.md'
blocked=$(printf '\153\141\155\151\171\157')
if rg --hidden -i -l --glob '!verify.sh' --glob '!node_modules/**' --glob '!dist/**' --glob '!.next/**' --glob '!target/**' "$blocked" $scope >/dev/null; then
  echo 'Blocked legacy term found' >&2
  exit 1
fi

if rg --hidden -i -n --glob '!verify.sh' --glob '!node_modules/**' --glob '!dist/**' --glob '!.next/**' --glob '!target/**' '\b(she|her|hers)\b' $scope; then
  echo 'Incorrect gendered language found' >&2
  exit 1
fi

identity_matches=$(rg --hidden -n --glob '!verify.sh' --glob '!node_modules/**' --glob '!dist/**' --glob '!.next/**' --glob '!target/**' '/Users/|[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' $scope || true)
identity_matches=$(printf '%s\n' "$identity_matches" | rg -v 'example\.com|users\.noreply\.github\.com|pnpm-lock\.yaml' || true)
if [ -n "$identity_matches" ]; then
  printf '%s\n' "$identity_matches" >&2
  echo 'Identity-sensitive content found' >&2
  exit 1
fi

pnpm exec prettier --ignore-path .gitignore --check \
  '.github/workflows/mizuki.yml' \
  '.github/workflows/mizuki-image.yml' \
  'apps/mizuki-web/**/*.{ts,tsx,js,mjs,json,css,md}' \
  '!apps/mizuki-web/next-env.d.ts' \
  'infra/mizuki/**/*.{yaml,md}' \
  'services/mizuki/**/*.{ts,mjs,json,md}' \
  'services/mizuki-policy-signer/**/*.{ts,json,md}' \
  'services/mizuki-updater/**/*.{ts,json,md}' \
  'services/mizuki-deployment-controller/**/*.{ts,json,md}' \
  'services/coding-gateway/**/*.{ts,json,md}' \
  services/coding-gateway/package.json package.json pnpm-workspace.yaml

audit_json=$(mktemp)
trap 'rm -f "$audit_json"' EXIT
pnpm audit --prod --json >"$audit_json" || true
ruby -rjson - "$audit_json" <<'RUBY'
data = JSON.parse(File.read(ARGV.fetch(0)))
prefixes = %w[
  apps__mizuki-web
  services__mizuki
  services__mizuki-policy-signer
  services__mizuki-updater
  services__mizuki-deployment-controller
  services__coding-gateway
]
blocked = data.fetch('advisories', {}).values.each_with_object([]) do |advisory, matches|
  next unless %w[moderate high critical].include?(advisory['severity'])

  paths = advisory.fetch('findings', []).flat_map { |finding| finding.fetch('paths', []) }
  scoped = paths.select do |path|
    prefixes.any? { |prefix| path == prefix || path.start_with?("#{prefix}>") }
  end
  next if scoped.empty?

  matches << "#{advisory['severity']}: #{advisory['module_name']} (#{scoped.join(', ')})"
end
unless blocked.empty?
  warn blocked.join("\n")
  abort 'Mizuki production dependency audit failed'
end
puts 'Mizuki production dependency audit OK'
RUBY

pnpm --filter @covenant/mizuki typecheck
pnpm --filter @covenant/mizuki test
pnpm --filter @covenant/mizuki build
pnpm --filter @covenant/mizuki smoke
pnpm --filter @covenant/mizuki-policy-signer typecheck
pnpm --filter @covenant/mizuki-policy-signer test
pnpm --filter @covenant/mizuki-policy-signer build
pnpm --filter @covenant/mizuki-updater typecheck
pnpm --filter @covenant/mizuki-updater test
pnpm --filter @covenant/mizuki-updater build
pnpm --filter @covenant/mizuki-deployment-controller typecheck
pnpm --filter @covenant/mizuki-deployment-controller test
pnpm --filter @covenant/mizuki-deployment-controller build
pnpm --filter @covenant/coding-gateway build
pnpm --filter @covenant/coding-gateway test
pnpm --filter @mizuki/web typecheck
pnpm --filter @mizuki/web test
pnpm --filter @mizuki/web build
pnpm --filter @mizuki/web smoke
if [ "${MIZUKI_SKIP_ESCROW:-0}" != '1' ]; then
  programs/mizuki-escrow/scripts/test.sh
fi

echo 'Mizuki launch artifacts OK'
