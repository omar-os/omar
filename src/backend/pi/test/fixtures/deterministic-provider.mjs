export default function deterministicProvider(pi) {
  pi.registerProvider("omar-pi-e2e", {
    baseUrl: process.env.PI_E2E_BASE_URL,
    apiKey: "pi-e2e-local",
    api: "openai-responses",
    models: [
      {
        id: "tree",
        name: "OMAR Pi E2E deterministic tool caller",
        reasoning: false,
        input: ["text"],
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
        contextWindow: 32768,
        maxTokens: 4096,
      },
    ],
  });
}
