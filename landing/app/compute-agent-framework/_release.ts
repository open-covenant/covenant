type AgentFrameworkRelease = {
  accepted: boolean;
  version: string;
  packageUrl: string | null;
  packageSha256: string | null;
  releaseUrl: string | null;
  sourceUrl: string | null;
  documentationUrl: string | null;
  demoSourceUrl: string | null;
  configurationUrl: string | null;
  qualifiedTests: number | null;
  acceptance: null | {
    evidenceUrl: string;
    paymentTransaction: string;
    customerPaymentUsdc: string;
    completedAt: string;
    resultSummary: string;
  };
};

const source = "https://github.com/open-covenant/covenant/blob/d85a26e6acaa36e8ed521c922a82f9fc04f8e93b";

// The installed wheel and funded mainnet workflow passed acceptance before the
// immutable release was published. The original output and review notes remain
// separate records; verification does not establish answer correctness.
export const agentFrameworkRelease: AgentFrameworkRelease = {
  accepted: true,
  version: "0.1.0",
  packageUrl: "https://github.com/open-covenant/covenant/releases/download/compute-x402-python-v0.1.0/covenant_compute_x402-0.1.0-py3-none-any.whl",
  packageSha256: "0613775ced5316871b2248fd9942b523cee548e8e53e8222e8db614165023325",
  releaseUrl: "https://github.com/open-covenant/covenant/releases/tag/compute-x402-python-v0.1.0",
  sourceUrl: `${source}/sdk/compute-x402-python/src/covenant_compute_x402/agent_framework.py`,
  documentationUrl: `${source}/sdk/compute-x402-python/README.md#microsoft-agent-framework`,
  demoSourceUrl: `${source}/sdk/compute-x402-python/src/covenant_compute_x402/workflow_demo.py`,
  configurationUrl: "/compute/agent-framework/client-config.example.json",
  qualifiedTests: 71,
  acceptance: {
    evidenceUrl: `${source}/docs/compute-agent-framework-release-evidence.md#funded-acceptance`,
    paymentTransaction: "5vqFrcJq25P7hDmdfuivuvsN7o8EfGSv53s4anttMoDUzf5LvVLze6CQQ4vNcxWn5r3kk1attctt2iVP1ZmwPj92",
    customerPaymentUsdc: "0.05",
    completedAt: "18 Oct 2026 · 19:37 UTC",
    resultSummary: "An incident-review draft with impact, hypotheses, missing evidence and follow-up recommendations.",
  },
};
