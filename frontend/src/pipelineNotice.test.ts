import { describe, expect, it } from "vitest";
import { briefError, selectPipelineNotice } from "./pipelineNotice";

/** Real error shapes from the backend (models.rs / pipeline.rs / Groq). */
const ENGINE_ERR =
  "whisper-cli is not installed — install whisper.cpp (macOS: brew install whisper-cpp; " +
  "Linux: build from source or use your package manager) or enable the Groq cloud fallback in Settings";
const DOWNLOAD_ERR =
  "download failed for whisper large-v3-turbo after trying 2 source(s) " +
  "(last error: download interrupted: unexpected end of file). Hugging Face's CDN sometimes " +
  "rejects downloads temporarily — retry later, pick an already-installed model in Settings, " +
  "or enable Groq cloud transcription.";
const GROQ_ERR = "groq returned 413 Payload Too Large: {}";

describe("selectPipelineNotice", () => {
  const base = { pipelineError: null, engineInstalling: false, engineInstalled: null };

  it("no error, nothing installing → none", () => {
    expect(selectPipelineNotice(base).kind).toBe("none");
  });

  it("an in-app install in flight always shows the engine notice", () => {
    expect(
      selectPipelineNotice({ ...base, engineInstalling: true, engineInstalled: false }).kind,
    ).toBe("engine-missing");
  });

  it("the engine resolver's own error → engine-missing", () => {
    const n = selectPipelineNotice({
      ...base,
      pipelineError: ENGINE_ERR,
      engineInstalled: false,
    });
    expect(n.kind).toBe("engine-missing");
    expect(n.installError).toBeNull();
  });

  it("engine error but the engine has appeared since (stale) → generic, not a lying Install prompt", () => {
    expect(
      selectPipelineNotice({ ...base, pipelineError: ENGINE_ERR, engineInstalled: true }).kind,
    ).toBe("generic");
  });

  it("a Groq failure with the engine absent is NOT mislabeled as engine-missing", () => {
    expect(
      selectPipelineNotice({ ...base, pipelineError: GROQ_ERR, engineInstalled: false }).kind,
    ).toBe("generic");
  });

  it("download failure with the engine fine → download-failed", () => {
    expect(
      selectPipelineNotice({ ...base, pipelineError: DOWNLOAD_ERR, engineInstalled: true }).kind,
    ).toBe("download-failed");
  });

  it("download failure while the engine is absent (failed install) → engine notice carrying the error", () => {
    const n = selectPipelineNotice({
      ...base,
      pipelineError: DOWNLOAD_ERR,
      engineInstalled: false,
    });
    expect(n.kind).toBe("engine-missing");
    expect(n.installError).toContain("download failed");
  });

  it("unknown error → generic", () => {
    expect(
      selectPipelineNotice({ ...base, pipelineError: "boom", engineInstalled: true }).kind,
    ).toBe("generic");
  });
});

describe("briefError", () => {
  it("strips signed CDN URLs and collapses whitespace", () => {
    const wall =
      "download failed: https://cdn-lfs.huggingface.co/repos/ab/cd/ggml.bin" +
      "?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIA%2F20260716&X-Amz-Signature=" +
      "deadbeef".repeat(24) +
      "   (status 403)";
    const out = briefError(wall);
    expect(out).toBe("download failed: (status 403)");
    expect(out).not.toMatch(/https?:/);
  });

  it("caps runaway text at the limit with an ellipsis", () => {
    const out = briefError("x".repeat(500));
    expect(out).toHaveLength(261); // 260 + ellipsis
    expect(out.endsWith("…")).toBe(true);
  });

  it("leaves short human text untouched", () => {
    expect(briefError("checksum mismatch (expected a, got b)")).toBe(
      "checksum mismatch (expected a, got b)",
    );
  });
});
