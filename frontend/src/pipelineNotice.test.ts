import { describe, expect, it } from "vitest";
import { briefError, pipelineFailureCopy, selectPipelineNotice } from "./pipelineNotice";

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

  it("a tagged install failure keeps the engine notice even without a download keyword", () => {
    const n = selectPipelineNotice({
      ...base,
      pipelineError: "engine install failed: unsupported archive format for whisper-bin-x64.7z",
      engineInstalled: false,
    });
    expect(n.kind).toBe("engine-missing");
    expect(n.installError).toContain("unsupported archive format");
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

describe("pipelineFailureCopy (notice stack, note not open)", () => {
  const base = {
    error: ENGINE_ERR,
    os: "windows",
    engineManaged: true,
    noteTitle: "Acme renewal",
    hadTranscript: false,
  } as const;

  it("first transcription: names the note, the recording is safe, ends at Try again", () => {
    const c = pipelineFailureCopy({ ...base, kind: "generic", error: "boom" });
    expect(c.title).toBe("Transcription didn’t finish");
    expect(c.cause).toBe("“Acme renewal” wasn’t transcribed. The recording is saved.");
    expect(c.steps).toEqual(["Choose Try again."]);
    expect(c.details).toBe("boom");
  });

  it("falls back to a generic subject when the note isn't known", () => {
    const c = pipelineFailureCopy({ ...base, kind: "generic", error: "boom", noteTitle: null });
    expect(c.cause.startsWith("A recording wasn’t transcribed.")).toBe(true);
  });

  it("a re-run (re-transcribe or re-analyze speakers) never says 'wasn't transcribed'", () => {
    const c = pipelineFailureCopy({ ...base, kind: "generic", error: "boom", hadTranscript: true });
    expect(c.title).toBe("Processing didn’t finish");
    expect(c.cause).toBe("“Acme renewal” didn’t finish processing. Its transcript is unchanged.");
    expect(c.steps).toEqual(["Open the note and run it again from its transcript."]);
    expect(JSON.stringify(c)).not.toMatch(/wasn’t transcribed|Try again/);
  });

  it("unknown history keeps both possibilities", () => {
    const c = pipelineFailureCopy({ ...base, kind: "generic", error: "x", hadTranscript: null });
    expect(c.cause).toContain("The recording and any transcript are kept.");
    expect(c.steps).toEqual(["Open the note and run it again from its transcript."]);
  });

  it("engine missing with a managed download → install in Settings, then Try again", () => {
    const c = pipelineFailureCopy({ ...base, kind: "engine-missing" });
    expect(c.title).toBe("Transcription engine isn’t installed");
    expect(c.steps).toEqual(["Install the engine in Settings › Transcripts.", "Choose Try again."]);
  });

  it("engine missing per OS when there's no managed download", () => {
    const mac = pipelineFailureCopy({
      ...base,
      kind: "engine-missing",
      os: "macos",
      engineManaged: false,
    });
    expect(mac.steps[0]).toBe("In Terminal, run: brew install whisper-cpp");
    const linux = pipelineFailureCopy({
      ...base,
      kind: "engine-missing",
      os: "linux",
      engineManaged: false,
    });
    expect(linux.steps[0]).toMatch(/package manager/);
    expect(linux.steps.join(" ")).not.toMatch(/brew/);
    const win = pipelineFailureCopy({ ...base, kind: "engine-missing", engineManaged: false });
    expect(win.steps[0]).toBe("Set up the engine in Settings › Transcripts.");
    for (const c of [mac, linux, win]) expect(c.steps[1]).toBe("Choose Try again.");
  });

  it("download failure: check the connection, then retry", () => {
    const first = pipelineFailureCopy({ ...base, kind: "download-failed", error: DOWNLOAD_ERR });
    expect(first.title).toBe("Model download didn’t finish");
    expect(first.steps).toEqual(["Check that you’re online.", "Choose Try again."]);
    // a re-analysis can need a model download too
    const rerun = pipelineFailureCopy({
      ...base,
      kind: "download-failed",
      error: DOWNLOAD_ERR,
      hadTranscript: true,
    });
    expect(rerun.cause).toContain("Its transcript is unchanged.");
    expect(rerun.steps[1]).toBe("Open the note and run it again from its transcript.");
  });

  it("details are briefed: no signed CDN URLs", () => {
    const c = pipelineFailureCopy({
      ...base,
      kind: "download-failed",
      error: "download failed: https://cdn-lfs.huggingface.co/x?X-Amz-Signature=abc (status 403)",
    });
    expect(c.details).toBe("download failed: (status 403)");
  });
});
