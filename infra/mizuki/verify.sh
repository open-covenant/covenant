#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$root"

ruby <<'RUBY'
require 'yaml'
require 'uri'
require 'json'
require 'digest'
require 'time'

def load_yaml(path)
  source = File.read(path)
  keyword_api = YAML.method(:safe_load).parameters.any? do |kind, _name|
    %i[key keyreq keyrest].include?(kind)
  end
  return YAML.safe_load(source, permitted_classes: [], permitted_symbols: [], aliases: false) if keyword_api

  YAML.safe_load(source, [], [], false)
end

blueprint = load_yaml('infra/mizuki/render.yaml')
bootstrap = load_yaml('infra/mizuki/render-bootstrap.yaml')
mizuki_workflow = load_yaml('.github/workflows/mizuki.yml')
ci_workflow = load_yaml('.github/workflows/ci.yml')
load_yaml('.github/workflows/mizuki-image.yml')
services = blueprint.fetch('services')
databases = blueprint.fetch('databases')
bootstrap_services = bootstrap.fetch('services')
bootstrap_databases = bootstrap.fetch('databases')
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

abort 'Mizuki workflow check names drift' unless mizuki_workflow.fetch('jobs').keys.sort == %w[application escrow escrow-release]
abort 'CI workflow check names drift' unless %w[landing rust].all? { |job| ci_workflow.fetch('jobs').key?(job) }
mizuki_triggers = mizuki_workflow['on'] || mizuki_workflow[true] || {}
ci_triggers = ci_workflow['on'] || ci_workflow[true] || {}
abort 'Mizuki workflow pull-request trigger missing' unless mizuki_triggers.key?('pull_request')
abort 'CI workflow pull-request trigger missing' unless ci_triggers.key?('pull_request')

abort 'Mizuki workflow has broad write permission' unless mizuki_workflow.fetch('permissions') == { 'contents' => 'read' }
workflow_jobs = mizuki_workflow.fetch('jobs')
escrow_job = workflow_jobs.fetch('escrow')
release_job = workflow_jobs.fetch('escrow-release')
release_condition = "github.event_name == 'push' && github.ref == 'refs/heads/main' && github.repository == 'open-covenant/covenant'"
abort 'escrow release is not restricted to protected main pushes' unless release_job.fetch('if') == release_condition
abort 'escrow release does not depend on the hosted build' unless release_job.fetch('needs') == 'escrow'
abort 'escrow release permission drift' unless release_job.fetch('permissions') == {
  'contents' => 'write',
  'id-token' => 'write',
  'attestations' => 'write',
  'artifact-metadata' => 'write'
}
abort 'read-only jobs gained permissions' if %w[application escrow].any? { |job| workflow_jobs.fetch(job).key?('permissions') }
abort 'main workflow can discard protected-main evidence' unless mizuki_workflow.dig('concurrency', 'group').include?('github.sha')
abort 'escrow release uses a step-only context at job scope' if release_job.fetch('env').values.any? { |value| value.include?('runner.temp') }

escrow_uses = escrow_job.fetch('steps').map { |step| step['uses'] }.compact
release_uses = release_job.fetch('steps').map { |step| step['uses'] }.compact
abort 'escrow artifact upload action drift' unless escrow_uses.include?('actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a')
abort 'escrow artifact download action drift' unless release_uses.include?('actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c')
abort 'escrow attestation action drift' unless release_uses.include?('actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6')

release_source = File.read('.github/workflows/mizuki.yml')
release_steps = release_job.fetch('steps').to_h { |step| [step.fetch('name'), step] }
inspect_source = release_steps.fetch('inspect immutable release').fetch('run')
publish_source = release_steps.fetch('publish immutable release').fetch('run')
verify_release_source = release_steps.fetch('verify immutable release and provenance').fetch('run')
abort 'escrow release can overwrite assets' if release_source.include?('--clobber')
abort 'escrow release uses an unsupported administration preflight' if release_source.include?('immutable-releases')
abort 'escrow release API version drift' unless release_source.include?('X-GitHub-Api-Version: 2026-03-10')
abort 'escrow release does not fail closed on mutable evidence' unless release_source.include?('.immutable == true')
abort 'escrow draft does not bind the exact source commit' unless release_source.include?('.target_commitish == $commit')
abort 'escrow release does not verify its exact source commit' unless release_source.include?('test "$tag_sha" = "$GITHUB_SHA"')
abort 'escrow release does not verify offline provenance' unless release_source.include?('gh attestation verify "$subject"') &&
  release_source.include?('--bundle "$REMOTE_DIR/GITHUB_PROVENANCE.json"')
abort 'escrow draft recovery does not enumerate authenticated releases' unless inspect_source.include?('repos/$GITHUB_REPOSITORY/releases?per_page=100') &&
  inspect_source.include?('echo "release-id=') && publish_source.include?('repos/$GITHUB_REPOSITORY/releases/$RELEASE_ID')
abort 'escrow immutable poll does not retry transient API responses' unless verify_release_source.include?('release_status=$?') &&
  verify_release_source.include?(%q[[ "$release_http" != '504' ]]) && verify_release_source.include?('immutable_ready=true')
abort 'escrow release does not verify the exact remote asset set' unless verify_release_source.include?('expected-assets') &&
  verify_release_source.include?('actual-assets') && verify_release_source.include?('.[0].digest == $digest')
draft_create = publish_source.index('-F draft=true')
asset_upload = publish_source.index('https://uploads.github.com/repos/$GITHUB_REPOSITORY/releases/$release_id/assets')
asset_digest = publish_source.index('.[0].digest == $digest')
publication = publish_source.index('-F draft=false')
immutability_poll = verify_release_source.index("for _ in 1 2 3 4 5 6 7 8 9 10; do")
abort 'escrow draft publication sequence drift' unless [draft_create, asset_upload, asset_digest, publication, immutability_poll].all? &&
  draft_create < asset_upload && asset_upload < asset_digest && asset_digest < publication

abort 'service set mismatch' unless services.map { |service| service.fetch('name') }.sort == expected.keys.sort
abort 'database set mismatch' unless databases.map { |database| database.fetch('name') }.sort == database_names.sort

bootstrap_service_names = %w[
  mizuki-policy-signer
  mizuki-coding-gateway
  mizuki-updater
]
bootstrap_database_names = %w[
  mizuki-signer-postgres
  mizuki-updater-postgres
]
abort 'bootstrap service set mismatch' unless bootstrap_services.map { |service| service.fetch('name') }.sort == bootstrap_service_names.sort
abort 'bootstrap database set mismatch' unless bootstrap_databases.map { |database| database.fetch('name') }.sort == bootstrap_database_names.sort

bootstrap_services.each do |service|
  name = service.fetch('name')
  production_service = services.find { |candidate| candidate.fetch('name') == name }
  abort "bootstrap service is missing from production: #{name}" unless production_service

  static = service.reject { |key, _value| key == 'envVars' }
  production_static = production_service.reject { |key, _value| key == 'envVars' }
  abort "bootstrap service definition drift: #{name}" unless static == production_static
  abort "bootstrap service branch is not protected main: #{name}" unless service.fetch('branch') == 'main'
  abort "bootstrap service has a public health check: #{name}" if service.key?('healthCheckPath')

  entries = service.fetch('envVars')
  keys = entries.map { |item| item.fetch('key') }
  abort "bootstrap duplicate environment key: #{name}" unless keys.uniq.size == keys.size
  entries.each do |item|
    sources = %w[value sync generateValue fromDatabase fromService].count { |key| item.key?(key) }
    abort "bootstrap invalid environment source: #{name}/#{item['key']}" unless sources == 1
    abort "bootstrap service reference is not closed: #{name}/#{item['key']}" if item.key?('fromService')
    next unless item['fromDatabase']

    database = item['fromDatabase']['name']
    abort "bootstrap unknown database reference: #{name}/#{item['key']}" unless bootstrap_database_names.include?(database)
  end

  production_env = production_service.fetch('envVars').to_h { |item| [item.fetch('key'), item] }
  bootstrap_env = entries.to_h { |item| [item.fetch('key'), item] }
  omitted = name == 'mizuki-updater' ? %w[MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN] : []
  abort "bootstrap environment set drift: #{name}" unless production_env.keys.sort == (bootstrap_env.keys + omitted).sort
  abort "bootstrap environment value drift: #{name}" unless bootstrap_env.all? { |key, value| production_env[key] == value }
end

bootstrap_databases.each do |database|
  production_database = databases.find { |candidate| candidate.fetch('name') == database.fetch('name') }
  abort "bootstrap database definition drift: #{database['name']}" unless production_database == database
end

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

shadow_keys = %w[
  NODE_ENV
  MIZUKI_RUNTIME_ROLE
  MIZUKI_HOST
  MIZUKI_PORT
  MIZUKI_PUBLIC_BASE_URL
  MIZUKI_TRUSTED_PROXY_HOPS
  MIZUKI_DATABASE_URL
  MIZUKI_ADMIN_TOKEN
  MIZUKI_WEB_PROXY_SECRET
  MIZUKI_PAYMENT_MODE
  MIZUKI_SESSION_SECRET
  MIZUKI_REQUIRE_GITHUB_APP
]
abort 'shadow runtime environment gained authority-bearing settings' unless shadow.keys.sort == shadow_keys.sort
abort 'shadow runtime role drift' unless shadow.fetch('MIZUKI_RUNTIME_ROLE')['value'] == 'shadow'
abort 'shadow runtime payment mode is not mock' unless shadow.fetch('MIZUKI_PAYMENT_MODE')['value'] == 'mock'
abort 'shadow runtime GitHub App requirement enabled' unless shadow.fetch('MIZUKI_REQUIRE_GITHUB_APP')['value'] == '0'
abort 'shadow runtime proxy trust drift' unless shadow.fetch('MIZUKI_TRUSTED_PROXY_HOPS')['value'] == '0'
abort 'shadow runtime private origin drift' unless shadow.fetch('MIZUKI_PUBLIC_BASE_URL')['value'] == 'http://mizuki-runtime-shadow:10000'
abort 'shadow runtime admin token is not isolated' unless shadow.fetch('MIZUKI_ADMIN_TOKEN')['generateValue'] == true
abort 'shadow runtime web proxy secret is not isolated' unless shadow.fetch('MIZUKI_WEB_PROXY_SECRET')['generateValue'] == true
abort 'shadow runtime session secret is not isolated' unless shadow.fetch('MIZUKI_SESSION_SECRET')['generateValue'] == true

abort 'production runtime role drift' unless production.fetch('MIZUKI_RUNTIME_ROLE')['value'] == 'production'
abort 'runtime Render proxy trust drift' unless production.fetch('MIZUKI_TRUSTED_PROXY_HOPS')['value'] == '1'
abort 'runtime rate-limit source capacity drift' unless production.fetch('MIZUKI_RATE_LIMIT_MAX_SOURCES')['value'] == '10000'
abort 'runtime SSE global cap drift' unless production.fetch('MIZUKI_SSE_MAX_CONNECTIONS')['value'] == '100'
abort 'runtime SSE source cap drift' unless production.fetch('MIZUKI_SSE_MAX_CONNECTIONS_PER_SOURCE')['value'] == '3'
abort 'runtime SSE idle timeout drift' unless production.fetch('MIZUKI_SSE_IDLE_TIMEOUT_MS')['value'] == '120000'
abort 'runtime readiness refresh drift' unless production.fetch('MIZUKI_READINESS_REFRESH_MS')['value'] == '30000'
abort 'runtime readiness max age drift' unless production.fetch('MIZUKI_READINESS_MAX_AGE_MS')['value'] == '90000'
abort 'runtime readiness timeout drift' unless production.fetch('MIZUKI_READINESS_TIMEOUT_MS')['value'] == '20000'
abort 'runtime escrow readiness floor drift' unless production.fetch('MIZUKI_ESCROW_READINESS_MIN_LAMPORTS')['value'] == '1000000000'
abort 'runtime signer URL is not private service discovery' unless service_ref(production.fetch('MIZUKI_POLICY_SIGNER_URL'), 'mizuki-policy-signer')
abort 'runtime signer token is not linked to signer' unless service_ref(production.fetch('MIZUKI_POLICY_SIGNER_TOKEN'), 'mizuki-policy-signer', 'MIZUKI_SIGNER_AUTH_TOKEN')
abort 'runtime gateway URL is not private service discovery' unless service_ref(production.fetch('MIZUKI_CODING_GATEWAY_URL'), 'mizuki-coding-gateway')
abort 'runtime gateway token is not linked to gateway' unless service_ref(production.fetch('MIZUKI_CODING_GATEWAY_TOKEN'), 'mizuki-coding-gateway', 'CODER_AUTH_TOKEN')
abort 'runtime updater URL is not private service discovery' unless service_ref(production.fetch('MIZUKI_UPDATER_URL'), 'mizuki-updater')
abort 'runtime updater token is not read-only' unless service_ref(production.fetch('MIZUKI_UPDATER_TOKEN'), 'mizuki-updater', 'MIZUKI_UPDATER_READ_TOKEN')
abort 'runtime updater timeout drift' unless production.fetch('MIZUKI_UPDATER_TIMEOUT_MS')['value'] == '15000'
abort 'runtime payment recipient is not the signer refund treasury' unless service_ref(production.fetch('MIZUKI_PAY_TO'), 'mizuki-policy-signer', 'MIZUKI_REFUND_TREASURY')
abort 'runtime escrow refund destination is not the isolated escrow authority' unless service_ref(production.fetch('MIZUKI_ESCROW_REFUND_TO'), 'mizuki-policy-signer', 'MIZUKI_ESCROW_AUTHORITY')
abort 'runtime x402 facilitator is not pinned to HTTPS' unless URI(production.fetch('MIZUKI_X402_FACILITATOR')['value']).scheme == 'https'
abort 'runtime UsePod origin drift' unless production.fetch('USEPOD_BASE_URL')['value'] == 'https://api.usepod.ai'
abort 'runtime coding route drift' unless production.fetch('USEPOD_MODEL')['value'] == 'openai/gpt-oss-120b'
abort 'runtime review route drift' unless production.fetch('USEPOD_REVIEW_MODEL')['value'] == 'deepseek-v4-flash'
abort 'runtime routes are not independent' if production.fetch('USEPOD_MODEL')['value'] == production.fetch('USEPOD_REVIEW_MODEL')['value']
abort 'runtime UsePod input ceiling drift' unless production.fetch('USEPOD_MAX_INPUT_PRICE_MICROUNITS')['value'] == '200000'
abort 'runtime UsePod output ceiling drift' unless production.fetch('USEPOD_MAX_OUTPUT_PRICE_MICROUNITS')['value'] == '400000'
abort 'runtime UsePod production floor drift' unless production.fetch('USEPOD_MIN_BALANCE')['value'] == '4000000'
abort 'runtime bounty review reservation drift' unless production.fetch('MIZUKI_BOUNTY_REVIEW_MAX_COST_MICROUNITS')['value'] == '50000'
abort 'runtime payment mode is not live' unless production.fetch('MIZUKI_PAYMENT_MODE')['value'] == 'live'
abort 'runtime GitHub App requirement disabled' unless production.fetch('MIZUKI_REQUIRE_GITHUB_APP')['value'] == '1'
abort 'runtime functional probe token is not production-only' unless service_ref(production.fetch('MIZUKI_RELEASE_PROBE_TOKEN'), 'mizuki-deployment-controller', 'MIZUKI_DEPLOY_PRODUCTION_PROBE_TOKEN')

abort 'production runtime is not bound to the canonical commercial database' unless production.fetch('MIZUKI_DATABASE_URL').dig('fromDatabase', 'name') == 'mizuki-postgres'
abort 'shadow runtime database not isolated' unless shadow.fetch('MIZUKI_DATABASE_URL').dig('fromDatabase', 'name') == 'mizuki-runtime-shadow-postgres'
abort 'runtime databases are shared' if production.fetch('MIZUKI_DATABASE_URL').dig('fromDatabase', 'name') == shadow.fetch('MIZUKI_DATABASE_URL').dig('fromDatabase', 'name')
abort 'production public origin does not follow its Render URL' unless service_ref(production.fetch('MIZUKI_PUBLIC_BASE_URL'), 'mizuki-runtime-production', 'RENDER_EXTERNAL_URL')
abort 'production web proxy secret is not generated' unless production.fetch('MIZUKI_WEB_PROXY_SECRET')['generateValue'] == true
abort 'production ClawPump payout is not linked to the escrow authority' unless service_ref(production.fetch('CLAWPUMP_PAYOUT_WALLET'), 'mizuki-policy-signer', 'MIZUKI_ESCROW_AUTHORITY')
abort 'production authority seed is not operator-pinned' unless production.fetch('MIZUKI_JOB_AUTHORITY_SEED')['sync'] == false
%w[
  USEPOD_API_KEY
  MIZUKI_GITHUB_APP_ID
  MIZUKI_GITHUB_PRIVATE_KEY
  MIZUKI_GITHUB_CLIENT_ID
  MIZUKI_GITHUB_CLIENT_SECRET
  MIZUKI_GITHUB_WEBHOOK_SECRET
].each do |key|
  abort "production runtime secret is not operator-pinned: #{key}" unless production.fetch(key)['sync'] == false
end

abort 'signer auth token is not generated' unless signer.fetch('MIZUKI_SIGNER_AUTH_TOKEN')['generateValue'] == true
abort 'refund key is not secret' unless signer.fetch('MIZUKI_REFUND_PRIVATE_KEY_JSON')['sync'] == false
abort 'escrow key is not secret' unless signer.fetch('MIZUKI_ESCROW_PRIVATE_KEY_JSON')['sync'] == false
abort 'signer verifier App ID is not operator-pinned' unless signer.fetch('MIZUKI_SIGNER_GITHUB_APP_ID')['sync'] == false
abort 'signer verifier App key is not secret' unless signer.fetch('MIZUKI_SIGNER_GITHUB_PRIVATE_KEY')['sync'] == false
abort 'stale signer repository credential setting present' if signer.key?('MIZUKI_SIGNER_GITHUB_TOKEN')
abort 'absolute liability registration expiry present' if signer.key?('MIZUKI_REFUND_LIABILITY_MAX_AGE_SECONDS')
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
abort 'deployment shadow probe URL drift' unless controller.fetch('MIZUKI_DEPLOY_SHADOW_PROBE_URL')['value'] == 'http://mizuki-runtime-shadow:10000/deployz'
abort 'deployment production probe URL is not operator-pinned' unless controller.fetch('MIZUKI_DEPLOY_PRODUCTION_PROBE_URL')['sync'] == false
abort 'deployment production probe token is not generated' unless controller.fetch('MIZUKI_DEPLOY_PRODUCTION_PROBE_TOKEN')['generateValue'] == true
abort 'legacy shared application probe token present' if controller.key?('MIZUKI_DEPLOY_PROBE_TOKEN')
abort 'deployment Render timeout drift' unless controller.fetch('MIZUKI_DEPLOY_RENDER_TIMEOUT_MS')['value'] == '20000'
abort 'deployment artifact timeout drift' unless controller.fetch('MIZUKI_DEPLOY_ARTIFACT_TIMEOUT_MS')['value'] == '30000'
abort 'deployment probe timeout drift' unless controller.fetch('MIZUKI_DEPLOY_PROBE_TIMEOUT_MS')['value'] == '10000'
application_probe_timeout = Integer(controller.fetch('MIZUKI_DEPLOY_PROBE_TIMEOUT_MS').fetch('value'), 10)
updater_timeout = Integer(production.fetch('MIZUKI_UPDATER_TIMEOUT_MS').fetch('value'), 10)
readiness_timeout = Integer(production.fetch('MIZUKI_READINESS_TIMEOUT_MS').fetch('value'), 10)
abort 'application/updater/readiness timeout ladder drift' unless application_probe_timeout < updater_timeout && updater_timeout < readiness_timeout
abort 'deployment reconciliation grace drift' unless controller.fetch('MIZUKI_DEPLOY_RECONCILIATION_GRACE_MS')['value'] == '120000'
abort 'deployment minimum promotion age drift' unless controller.fetch('MIZUKI_DEPLOY_MIN_PROMOTION_AGE_MS')['value'] == '10800000'
updater_authorities = %w[MIZUKI_UPDATER_SUBMIT_TOKEN MIZUKI_UPDATER_CONTROL_TOKEN MIZUKI_UPDATER_READ_TOKEN]
abort 'updater role-specific tokens are missing' unless updater_authorities.all? { |key| updater.key?(key) }
abort 'legacy shared updater authority present' if updater.key?('MIZUKI_UPDATER_AUTH_TOKEN')
controller_ref = updater.fetch('MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT').fetch('fromService')
abort 'updater controller origin is not private service discovery' unless controller_ref == {
  'type' => 'pserv',
  'name' => 'mizuki-deployment-controller',
  'property' => 'hostport'
}
abort 'updater controller token is not linked' unless service_ref(updater.fetch('MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN'), 'mizuki-deployment-controller', 'MIZUKI_DEPLOY_AUTH_TOKEN')
abort 'updater hook timeout drift' unless updater.fetch('MIZUKI_UPDATER_HOOK_TIMEOUT_MS')['value'] == '90000'
abort 'updater lease does not safely exceed hook duration' unless updater.fetch('MIZUKI_UPDATER_LEASE_MS')['value'] == '180000'
abort 'updater retry horizon drift' unless updater.fetch('MIZUKI_UPDATER_MAX_ATTEMPTS')['value'] == '10'
abort 'updater promotion soak drift' unless updater.fetch('MIZUKI_UPDATER_PROMOTION_SOAK_MS')['value'] == '10800000'
abort 'updater promotion timeout drift' unless updater.fetch('MIZUKI_UPDATER_PROMOTION_TIMEOUT_MS')['value'] == '11400000'
abort 'updater base branch drift' unless updater.fetch('MIZUKI_UPDATER_ALLOWED_BASE_BRANCHES')['value'] == 'main'
abort 'updater artifact origin drift' unless updater.fetch('MIZUKI_UPDATER_ARTIFACT_ORIGINS')['value'] == 'https://github.com,https://release-assets.githubusercontent.com'
abort 'updater mandatory checks drift' unless updater.fetch('MIZUKI_UPDATER_MANDATORY_CHECKS')['value'] == 'application,escrow,rust,landing'
expected_check_producers = %w[application escrow rust landing].to_h do |check|
  application_check = %w[application escrow].include?(check)
  [check, {
    'checkRunAppId' => 15_368,
    'workflowId' => application_check ? 340_541_049 : 265_742_803,
    'workflowPath' => application_check ? '.github/workflows/mizuki.yml' : '.github/workflows/ci.yml',
    'event' => 'pull_request',
    'headBranch' => 'manifest',
    'headSha' => 'candidate',
    'baseBranch' => 'manifest',
    'baseSha' => 'signed',
    'definitionRef' => 'base'
  }]
end
actual_check_producers = JSON.parse(updater.fetch('MIZUKI_UPDATER_CHECK_PRODUCERS_JSON').fetch('value'))
abort 'updater check producer policy drift' unless actual_check_producers == expected_check_producers
bootstrap_updater = bootstrap_services
  .find { |service| service.fetch('name') == 'mizuki-updater' }
  .fetch('envVars')
  .to_h { |item| [item.fetch('key'), item] }
bootstrap_check_producers = JSON.parse(bootstrap_updater.fetch('MIZUKI_UPDATER_CHECK_PRODUCERS_JSON').fetch('value'))
abort 'bootstrap updater check producer policy drift' unless bootstrap_check_producers == expected_check_producers

disk = services.find { |service| service['name'] == 'mizuki-coding-gateway' }.fetch('disk')
abort 'gateway disk is not mounted at /var/data' unless disk['mountPath'] == '/var/data' && disk['sizeGB'].to_i >= 1
abort 'gateway is not pinned to UsePod' unless gateway.fetch('CODER_BACKEND')['value'] == 'usepod'
abort 'gateway has an ambiguous legacy route setting' if gateway.key?('USEPOD_MODEL')
abort 'gateway coding route drift' unless gateway.fetch('CODER_MODEL')['value'] == 'openai/gpt-oss-120b'
abort 'gateway UsePod token is not secret' unless gateway.fetch('USEPOD_API_KEY')['sync'] == false
abort 'gateway auth token is not generated' unless gateway.fetch('CODER_AUTH_TOKEN')['generateValue'] == true
abort 'gateway spend ledger is not persistent' unless gateway.fetch('LEDGER_PATH')['value'].start_with?('/var/data/')
abort 'gateway run store is not persistent' unless gateway.fetch('RUN_STORE_PATH')['value'].start_with?('/var/data/')
abort 'gateway readiness refresh drift' unless gateway.fetch('CODER_READINESS_REFRESH_MS')['value'] == '120000'
abort 'gateway readiness max age drift' unless gateway.fetch('CODER_READINESS_MAX_AGE_MS')['value'] == '300000'
abort 'gateway readiness timeout drift' unless gateway.fetch('CODER_READINESS_TIMEOUT_MS')['value'] == '20000'
abort 'gateway retains mutable E2B template alias' if gateway.key?('E2B_TEMPLATE')
abort 'gateway retains legacy sandbox estimate' if gateway.key?('CODER_SANDBOX_USD_PER_SEC')
abort 'gateway E2B template ID drift' unless gateway.fetch('E2B_TEMPLATE_ID')['value'] == 'aaj2iho3gnyf5fcvln83'
abort 'gateway E2B CPU identity drift' unless gateway.fetch('E2B_EXPECTED_CPU_COUNT')['value'] == '4'
abort 'gateway E2B memory identity drift' unless gateway.fetch('E2B_EXPECTED_MEMORY_MB')['value'] == '4096'
abort 'gateway E2B worst-case tariff drift' unless gateway.fetch('CODER_E2B_WORST_CASE_USD_PER_SEC')['value'] == '0.0002'
abort 'gateway UsePod origin drift' unless gateway.fetch('USEPOD_BASE_URL')['value'] == 'https://api.usepod.ai'
abort 'gateway UsePod input ceiling drift' unless gateway.fetch('USEPOD_MAX_INPUT_PRICE_MICROUNITS')['value'] == '200000'
abort 'gateway UsePod output ceiling drift' unless gateway.fetch('USEPOD_MAX_OUTPUT_PRICE_MICROUNITS')['value'] == '400000'
abort 'gateway UsePod production floor drift' unless gateway.fetch('USEPOD_MIN_BALANCE')['value'] == '4000000'

route_evidence_path = 'infra/mizuki/evidence/usepod-route-2026-08-23.json'
route_evidence_raw = File.binread(route_evidence_path)
route_evidence = JSON.parse(route_evidence_raw)
route_evidence_digest = Digest::SHA256.hexdigest(route_evidence_raw)
expected_route_evidence_digest = '21bbff5860332305ec090c9bb8245de36e6e53819a97d29401724c5c3644c441'
route_evidence_ref = "https://raw.githubusercontent.com/open-covenant/covenant/main/#{route_evidence_path}#sha256=#{route_evidence_digest}"
abort 'UsePod route evidence digest drift' unless route_evidence_digest == expected_route_evidence_digest
abort 'environment contract route evidence reference drift' unless File.read('infra/mizuki/env-contract.md').include?(route_evidence_ref)
abort 'deployment route evidence reference drift' unless File.read('infra/mizuki/deployment-evidence.md').include?(route_evidence_ref)
abort 'UsePod route evidence schema drift' unless route_evidence['schema'] == 'mizuki.usepod-route-evidence.v1'
abort 'UsePod route evidence timestamp drift' unless route_evidence['capturedAt'] == '2026-08-23T20:47:54Z'
funding = route_evidence.fetch('funding')
abort 'UsePod funding network drift' unless funding['network'] == 'solana-mainnet' && funding['commitment'] == 'finalized'
abort 'UsePod funding method drift' unless funding['method'] == 'DepositUsdc'
abort 'UsePod funding asset drift' unless funding['asset'] == 'USDC' && funding['mint'] == 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v'
abort 'UsePod funding amount drift' unless funding['amountMicrounits'] == 50000
abort 'UsePod sovereign program drift' unless funding['sovereignProgram'] == 'BBAdcqUkg68JXNiPQ1HR1wujfZuayyK3eQTQSYAh6FSW'
abort 'UsePod funding signature drift' unless funding['signature'] == 'ExRVdguFoDeHTCF9P1yfKozcpxML9Y4s1WdzYTFDdeRcMVkjVGwP4qwmYJpPc4DBwtcbuQwt3QdTNh4KdzRo3ih'
abort 'UsePod funding slot drift' unless funding['slot'] == 441239653 && funding['blockTime'] == '2026-08-23T20:47:54Z'
account = route_evidence.fetch('account')
abort 'UsePod account activation evidence drift' unless account['tokenActivated'] == true && account['catalogModelCount'] == 1013
abort 'UsePod canary funding evidence drift' unless account['canaryFundingMicrounits'] == 50000
abort 'UsePod production floor evidence drift' unless account['productionBalanceFloorMicrounits'] == 4000000 && account['productionBalanceFloorMet'] == false
coding_route = route_evidence.dig('routes', 'coding')
abort 'UsePod coding canary route drift' unless coding_route['model'] == 'openai/gpt-oss-120b' && coding_route['route'] == 'marketplace'
abort 'UsePod coding canary tool proof drift' unless coding_route.dig('toolCall', 'forced') == true && coding_route.dig('toolCall', 'nonceMatched') == true
abort 'UsePod coding canary balance evidence drift' unless coding_route.dig('headers', 'balanceHeaderValidPositive') == true
abort 'UsePod coding canary provider evidence drift' unless coding_route.dig('headers', 'providerIdPresent') == true
abort 'UsePod coding canary cost evidence is overstated' unless coding_route.dig('headers', 'providerCostPresent') == false && coding_route.dig('headers', 'providerRequestIdPresent') == false
abort 'UsePod coding canary usage drift' unless coding_route['usage'] == { 'inputTokens' => 147, 'outputTokens' => 61 }
review_route = route_evidence.dig('routes', 'review')
abort 'UsePod review canary route drift' unless review_route['model'] == 'deepseek-v4-flash' && review_route['route'] == 'marketplace'
abort 'UsePod review canary output drift' unless review_route['strictDecisionJsonParsed'] == true && review_route['maxTokens'] == 512
abort 'UsePod review canary balance evidence drift' unless review_route.dig('headers', 'balanceHeaderValidPositive') == true
abort 'UsePod review canary provider evidence drift' unless review_route.dig('headers', 'providerIdPresent') == true
abort 'UsePod review canary cost evidence is overstated' unless review_route.dig('headers', 'providerCostPresent') == false && review_route.dig('headers', 'providerRequestIdPresent') == false
abort 'UsePod review canary usage drift' unless review_route['usage'] == { 'inputTokens' => 1177, 'outputTokens' => 493 }
abort 'UsePod rejected route evidence drift' unless route_evidence['rejectedCandidates'] == [
  {
    'requestedModel' => 'deepseek-v3.2',
    'result' => 'canonicalized-and-failed-tool-call'
  },
  {
    'requestedModel' => 'deepseek/deepseek-v3.2',
    'result' => 'failed-tool-call-and-conflicting-duplicate-balance-values'
  }
]
abort 'UsePod route evidence launch verdict drift' unless route_evidence['launchVerdict'] == 'no-go-paid-intake'
serialized_route_evidence = JSON.generate(route_evidence)
%w[token depositCode balance providerId apiKey secret credential].each do |forbidden_key|
  abort "UsePod route evidence exposes #{forbidden_key}" if serialized_route_evidence.match?(/\"#{Regexp.escape(forbidden_key)}\"\s*:/i)
end

preflight_path = 'infra/mizuki/evidence/mainnet-preflight-2026-08-23.json'
preflight_raw = File.binread(preflight_path)
preflight = JSON.parse(preflight_raw)
preflight_digest = Digest::SHA256.hexdigest(preflight_raw)
expected_preflight_digest = '8debdc14b45ec698f0af45f1a758036d5c343bf3abe4c66fc2174fde17cbe70e'
preflight_ref = "https://raw.githubusercontent.com/open-covenant/covenant/main/#{preflight_path}#sha256=#{preflight_digest}"
abort 'mainnet preflight evidence digest drift' unless preflight_digest == expected_preflight_digest
abort 'deployment preflight evidence reference drift' unless File.read('infra/mizuki/deployment-evidence.md').include?(preflight_ref)
abort 'mainnet preflight schema drift' unless preflight['schema'] == 'mizuki.mainnet-preflight.v1'
abort 'mainnet preflight network drift' unless preflight['network'] == 'solana-mainnet' && preflight['commitment'] == 'finalized'
abort 'mainnet preflight program result drift' unless preflight.dig('program', 'accountExists') == false
abort 'mainnet preflight account result drift' unless preflight.fetch('accounts').length == 4 && preflight['accounts'].all? do |account|
  account['lamports'] == 0 && account['usdcTokenAccountExists'] == false
end
abort 'mainnet preflight provider independence drift' unless preflight.fetch('observations').map { |observation| URI(observation.fetch('rpc')).host }.uniq == [
  'api.mainnet-beta.solana.com',
  'solana-rpc.publicnode.com'
]
abort 'mainnet preflight release floor drift' unless preflight.dig('requiredBeforeDeploy', 'minimumDeployerLamports') == 750_000_000
abort 'mainnet preflight overstates release readiness' unless preflight['launchVerdict'] == 'no-go-mainnet-deploy' &&
  preflight.dig('requiredBeforeDeploy', 'independentReview') == false &&
  preflight.dig('requiredBeforeDeploy', 'hostedReproducibleBuildForCurrentRevision') == false &&
  preflight.dig('requiredBeforeDeploy', 'twoRpcAgreementAfterFinalDeployment') == false

readiness_path = 'infra/mizuki/evidence/mainnet-readiness-2026-08-24.json'
readiness_raw = File.binread(readiness_path)
readiness = JSON.parse(readiness_raw)
readiness_digest = Digest::SHA256.hexdigest(readiness_raw)
expected_readiness_digest = 'e345f8249d16671c4474542c9ad8b6d9d06b95cea231821f7a74a2bad40d77e9'
readiness_ref = "https://raw.githubusercontent.com/open-covenant/covenant/main/#{readiness_path}#sha256=#{readiness_digest}"
abort 'mainnet readiness evidence digest drift' unless readiness_digest == expected_readiness_digest
abort 'deployment readiness evidence reference drift' unless File.read('infra/mizuki/deployment-evidence.md').include?(readiness_ref)
abort 'mainnet readiness schema drift' unless readiness['schema'] == 'mizuki.mainnet-readiness.v1'
abort 'mainnet readiness timestamp drift' unless readiness['capturedAt'] == '2026-08-24T01:02:55Z'
abort 'mainnet readiness network drift' unless readiness['network'] == 'solana-mainnet' && readiness['commitment'] == 'finalized'
abort 'mainnet readiness genesis drift' unless readiness['canonicalGenesisHash'] == '5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d'
readiness_accounts = readiness.fetch('accounts')
abort 'mainnet readiness program drift' unless readiness_accounts['program'] == preflight.dig('program', 'address')
abort 'mainnet readiness authority drift' unless [
  readiness_accounts['releaseDeployer'],
  readiness_accounts['refundTreasury'],
  readiness_accounts['escrowAuthority'],
  readiness_accounts['jobAuthority']
] == preflight.fetch('accounts').map { |entry| entry.fetch('address') }
abort 'mainnet readiness token-account drift' unless readiness_accounts['canonicalUsdcAccounts'] == preflight.fetch('accounts').map { |entry| entry.fetch('usdcTokenAccount') }
abort 'mainnet readiness overstates account state' unless readiness_accounts['allAbsent'] == true
readiness_observations = readiness.fetch('observations')
abort 'mainnet readiness observation count drift' unless readiness_observations.length == 2
abort 'mainnet readiness provider independence drift' unless readiness_observations.map { |observation| URI(observation.fetch('rpc')).host } == [
  'api.mainnet-beta.solana.com',
  'solana-rpc.publicnode.com'
]
abort 'mainnet readiness observation drift' unless readiness_observations.all? do |observation|
  observation['slot'] == 441_281_187 && observation['genesisMatch'] == true && observation['result'] == 'all-nine-accounts-absent'
end
deployment_rent = readiness.fetch('deploymentRent')
abort 'mainnet readiness program-data rent drift' unless deployment_rent['programDataBytes'] == 104_421 && deployment_rent['programDataLamports'] == 727_661_040
abort 'mainnet readiness loader rent drift' unless deployment_rent['programAccountBytes'] == 36 && deployment_rent['programAccountLamports'] == 1_141_440
abort 'mainnet readiness combined rent drift' unless deployment_rent['combinedPermanentLamports'] == deployment_rent['programDataLamports'] + deployment_rent['programAccountLamports']
abort 'mainnet readiness deployer floor drift' unless deployment_rent['minimumDeployerLamports'] == 750_000_000 && deployment_rent['deployerLamports'] == 0
facilitator = readiness.fetch('facilitator')
abort 'mainnet readiness facilitator drift' unless facilitator['url'] == 'https://facilitator.payai.network' && facilitator['x402Version'] == 2 && facilitator['scheme'] == 'exact'
abort 'mainnet readiness facilitator network drift' unless facilitator['network'] == 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp'
abort 'mainnet readiness facilitator route drift' unless facilitator['matchingRouteCount'] == 1 && facilitator['advertisedSignerCount'] == 2
abort 'mainnet readiness facilitator fee payer drift' unless facilitator['feePayer'] == 'CjNFTjvBhbJJd2B5ePPMHRLx1ELZpa8dwQgGL727eKww' && facilitator['feePayerLamports'] == 2_956_725_332
abort 'mainnet readiness facilitator observations drift' unless facilitator.fetch('balanceObservations').map { |observation| URI(observation.fetch('rpc')).host } == [
  'api.mainnet-beta.solana.com',
  'solana-rpc.publicnode.com'
]
marketplace = readiness.fetch('marketplace')
abort 'mainnet readiness marketplace deposit drift' unless marketplace['historicalDepositSignature'] == funding['signature'] && marketplace['historicalDepositSlot'] == funding['slot'] && marketplace['historicalDepositMicrounits'] == funding['amountMicrounits']
abort 'mainnet readiness marketplace floor drift' unless marketplace['productionFloorMicrounits'] == 4_000_000 && marketplace['minimumHistoricalShortfallMicrounits'] == 3_950_000
abort 'mainnet readiness overstates marketplace balance' unless marketplace['currentBalanceVerifiable'] == false
readiness_tariff = readiness.fetch('sandboxTariff')
abort 'mainnet readiness tariff digest drift' unless readiness_tariff['sourceSha256'] == '28e0e81c35b2d6e8def4bab24d105e5b39d31330c39be20f5411b51df664bbc7'
abort 'mainnet readiness overstates sandbox readiness' unless readiness_tariff['authenticatedTemplateVerified'] == false && readiness_tariff['fundingVerified'] == false
abort 'mainnet readiness overstates launch readiness' unless readiness['launchVerdict'] == 'no-go-mainnet-and-paid-intake'

tariff_path = 'infra/mizuki/evidence/e2b-tariff-2026-08-23.json'
tariff_raw = File.binread(tariff_path)
tariff = JSON.parse(tariff_raw)
tariff_digest = Digest::SHA256.hexdigest(tariff_raw)
tariff_ref = "https://raw.githubusercontent.com/open-covenant/covenant/main/#{tariff_path}#sha256=#{tariff_digest}"
abort 'gateway tariff evidence reference drift' unless gateway.fetch('CODER_E2B_TARIFF_REF')['value'] == tariff_ref
abort 'gateway tariff evidence schema drift' unless tariff['schema'] == 'mizuki.e2b-tariff.v1' && tariff['provider'] == 'e2b'
abort 'gateway tariff evidence source drift' unless tariff['sourceUrl'] == 'https://e2b.dev/pricing'
abort 'gateway tariff source digest is invalid' unless tariff['sourceSha256'].match?(/\A[0-9a-f]{64}\z/)
abort 'gateway tariff template identity drift' unless tariff['templateId'] == 'aaj2iho3gnyf5fcvln83'
abort 'gateway tariff resource identity drift' unless tariff['cpuCount'] == 4 && tariff['memoryMb'] == 4096
abort 'gateway tariff CPU rate drift' unless tariff['cpuUsdPerCoreSecond'] == 0.000014
abort 'gateway tariff memory rate drift' unless tariff['memoryUsdPerGibSecond'] == 0.0000045
abort 'gateway tariff fixed rate drift' unless tariff['fixedUsdPerSecond'] == 0
abort 'gateway tariff safety multiplier drift' unless tariff['safetyMultiplier'] == 2
abort 'gateway tariff worst-case rate drift' unless tariff['worstCaseUsdPerSecond'] == 0.0002
effective_at = Time.iso8601(tariff.fetch('effectiveAt'))
valid_until = Time.iso8601(tariff.fetch('validUntil'))
validity_seconds = valid_until - effective_at
abort 'gateway tariff validity window is invalid' unless validity_seconds.positive? && validity_seconds <= 7 * 24 * 60 * 60
base_rate = tariff['fixedUsdPerSecond'] + tariff['cpuCount'] * tariff['cpuUsdPerCoreSecond'] + (tariff['memoryMb'] / 1024.0) * tariff['memoryUsdPerGibSecond']
abort 'gateway tariff does not cover the declared formula' if tariff['worstCaseUsdPerSecond'] < base_rate * tariff['safetyMultiplier']

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
  'metadata' => 'read',
  'pull_requests' => 'write'
}
abort 'policy verifier App must be public' unless verifier_app['public'] == true
abort 'policy verifier App permission drift' unless verifier_app['default_permissions'] == {
  'checks' => 'read',
  'contents' => 'read',
  'issues' => 'read',
  'metadata' => 'read',
  'pull_requests' => 'read',
  'statuses' => 'read'
}
abort 'policy verifier App must not subscribe to events' unless verifier_app['default_events'] == []
abort 'updater App must remain private' unless updater_app['public'] == false
abort 'updater App permission drift' unless updater_app['default_permissions'] == {
  'actions' => 'read',
  'checks' => 'read',
  'contents' => 'write',
  'metadata' => 'read',
  'pull_requests' => 'write'
}
abort 'updater App must not subscribe to events' unless updater_app['default_events'] == []

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
  'infra/mizuki/**/*.{yaml,md,json}' \
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
