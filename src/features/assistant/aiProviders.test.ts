import { describe, expect, it } from "vitest";
import { apiKeyProblem, baseUrlOf, endpointProblem, providerPreset } from "./aiProviders";

describe("endpointProblem", () => {
  it("accepts an empty endpoint for built-in providers (their public URL is used)", () => {
    expect(endpointProblem("", "openai")).toBeNull();
    expect(endpointProblem("  ", "anthropic")).toBeNull();
  });

  it("requires an endpoint for a custom provider", () => {
    expect(endpointProblem("", "custom")).toBe("Enter the endpoint URL of your server, e.g. http://localhost:11434/v1");
  });

  it("accepts http and https URLs", () => {
    expect(endpointProblem("http://localhost:11434/v1", "custom")).toBeNull();
    expect(endpointProblem(" HTTPS://gateway.example.com/v1 ", "fosta")).toBeNull();
  });

  it("rejects anything that is not an http(s) URL, quoting at most 40 characters", () => {
    expect(endpointProblem("localhost:11434", "custom")).toBe('Endpoint must be a URL starting with http:// or https://, not "localhost:11434"');
    expect(endpointProblem("https://has space", "custom")).toMatch(/^Endpoint must be a URL/);
    const long = "x".repeat(60);
    expect(endpointProblem(long, "custom")).toBe(`Endpoint must be a URL starting with http:// or https://, not "${"x".repeat(40)}"`);
  });
});

describe("baseUrlOf / providerPreset", () => {
  it("uses the provider's public URL when none is set and trims trailing slashes", () => {
    expect(baseUrlOf({ provider: "openai", baseUrl: "", model: "m" })).toBe("https://api.openai.com/v1");
    expect(baseUrlOf({ provider: "custom", baseUrl: "http://localhost:11434/v1//", model: "m" })).toBe("http://localhost:11434/v1");
  });

  it("falls back to the first preset for unknown providers", () => {
    expect(providerPreset("nope").value).toBe("fosta");
    expect(providerPreset(undefined).value).toBe("fosta");
  });
});

describe("apiKeyProblem", () => {
  it("requires a key for hosted providers", () => {
    expect(apiKeyProblem("fosta", "", false)).toBe("Fosta needs an API key. Fosta gateway. Key from admin.fosta.app.");
    expect(apiKeyProblem("openai", "  ", false)).not.toBeNull();
  });

  it("accepts a typed key, a saved key, or a custom endpoint without one", () => {
    expect(apiKeyProblem("fosta", "sk-1", false)).toBeNull();
    expect(apiKeyProblem("anthropic", "", true)).toBeNull();
    expect(apiKeyProblem("custom", "", false)).toBeNull();
  });
});
