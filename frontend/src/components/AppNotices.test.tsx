// @vitest-environment jsdom
import { act } from "react";
import type { ComponentProps, ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Updater, UpdatePhase } from "../updater";
import type { PipelineFailure } from "../pipelineFailures";
import AppNotices from "./AppNotices";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let mounted: { root: Root; host: HTMLElement }[] = [];

function render(ui: ReactNode) {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  act(() => root.render(ui));
  mounted.push({ root, host });
  return { host, rerender: (next: ReactNode) => act(() => root.render(next)) };
}

afterEach(() => {
  for (const { root, host } of mounted) {
    act(() => root.unmount());
    host.remove();
  }
  mounted = [];
});

const click = (el: Element | null | undefined) => {
  if (!el) throw new Error("element not found");
  act(() => {
    (el as HTMLElement).click();
  });
};

const button = (host: HTMLElement, label: string) =>
  Array.from(host.querySelectorAll("button")).find((b) => b.textContent?.trim() === label);

function fakeUpdater(phase: UpdatePhase, over: Partial<Updater> = {}): Updater {
  return {
    phase,
    version: "2.4.0",
    progress: null,
    error: null,
    supported: true,
    dismissed: false,
    check: vi.fn(),
    downloadAndInstall: vi.fn(),
    restart: vi.fn(),
    dismiss: vi.fn(),
    ...over,
  };
}

const FAILURE: PipelineFailure = {
  meetingId: "m2",
  noteId: "n2",
  noteTitle: "Weekly 1:1 with Dana",
  error: "groq returned 413 Payload Too Large",
  hadTranscript: false,
};

type Props = ComponentProps<typeof AppNotices>;

function props(over: Partial<Props> = {}): Props {
  return {
    os: "macos",
    recordingActive: false,
    systemAudioBlocked: false,
    onOpenPrivacySettings: vi.fn(),
    onDismissSystemAudio: vi.fn(),
    appError: null,
    onDismissAppError: vi.fn(),
    pipelineFailures: [],
    engine: { installed: true, managed: true },
    onOpenFailedNote: vi.fn(),
    onRetryFailure: vi.fn(),
    onDismissPipelineFailure: vi.fn(),
    onOpenEngineSettings: vi.fn(),
    updater: fakeUpdater("idle"),
    ...over,
  };
}

/** Ids of the notices currently rendered, top first. */
const shownIds = (host: HTMLElement) =>
  Array.from(host.querySelectorAll("[data-notice-id]")).map((el) =>
    el.getAttribute("data-notice-id"),
  );

describe("AppNotices", () => {
  it("renders nothing with no notices", () => {
    const { host } = render(<AppNotices {...props()} />);
    expect(host.querySelector('[role="region"]')).toBeNull();
  });

  it("all four at once: the failed action on top, the rest behind +3 more, in priority order", () => {
    const { host } = render(
      <AppNotices
        {...props({
          systemAudioBlocked: true,
          appError: "Microphone is busy",
          pipelineFailures: [FAILURE],
          updater: fakeUpdater("available"),
        })}
      />,
    );
    expect(shownIds(host)).toEqual(["app-error"]);
    // its alert renders (not hidden behind "+N more")
    expect(host.querySelector('[role="alert"]')?.textContent).toContain("Microphone is busy");
    const more = host.querySelector(".fly-notices-more");
    expect(more?.textContent).toBe("+3 more");
    click(more);
    expect(shownIds(host)).toEqual(["app-error", "system-audio", "pipeline:m2", "update"]);
    expect(host.querySelector(".fly-notices-more")?.textContent).toBe("Show fewer");
  });

  it.each([
    [{ appError: "x", updater: fakeUpdater("available") }, ["app-error"], "+1 more"],
    // a stale macOS grant must not hide a failed Record start
    [{ systemAudioBlocked: true, appError: "x" }, ["app-error"], "+1 more"],
    [{ appError: "x", pipelineFailures: [FAILURE] }, ["app-error"], "+1 more"],
    [{ pipelineFailures: [FAILURE], updater: fakeUpdater("ready") }, ["pipeline:m2"], "+1 more"],
    [{ systemAudioBlocked: true, updater: fakeUpdater("ready") }, ["system-audio"], "+1 more"],
    [{ systemAudioBlocked: true, pipelineFailures: [FAILURE] }, ["system-audio"], "+1 more"],
    [{ updater: fakeUpdater("available") }, ["update"], null],
  ] as const)("pair %#: the higher priority shows first", (over, top, more) => {
    const { host } = render(<AppNotices {...props(over as Partial<Props>)} />);
    expect(shownIds(host)).toEqual(top);
    expect(host.querySelector(".fly-notices-more")?.textContent ?? null).toBe(more);
  });

  it("several failures each get their own notice", () => {
    const second = { ...FAILURE, meetingId: "m4", noteId: "n4", noteTitle: "Q3 planning" };
    const { host } = render(<AppNotices {...props({ pipelineFailures: [FAILURE, second] })} />);
    click(host.querySelector(".fly-notices-more"));
    expect(shownIds(host)).toEqual(["pipeline:m2", "pipeline:m4"]);
  });

  it("while recording, system audio and the update wait; errors still show", () => {
    const { host } = render(
      <AppNotices
        {...props({
          recordingActive: true,
          systemAudioBlocked: true,
          appError: "Couldn't save",
          updater: fakeUpdater("available"),
        })}
      />,
    );
    expect(shownIds(host)).toEqual(["app-error"]);
    expect(host.querySelector(".fly-notices-more")).toBeNull();
  });

  it("macOS system audio: numbered fix, tccutil under Still blocked?, opens System Settings", () => {
    const p = props({ systemAudioBlocked: true });
    const { host } = render(<AppNotices {...p} />);
    const notice = host.querySelector('[data-notice-id="system-audio"] .fly-notice');
    expect(notice?.getAttribute("role")).toBe("alert");
    expect(host.querySelectorAll(".fly-notice-steps li")).toHaveLength(3);
    expect(host.querySelector(".fly-notice-details summary")?.textContent).toBe("Still blocked?");
    expect(host.querySelector(".fly-notice-details-body")?.textContent).toContain("tccutil");
    click(button(host, "Open System Settings"));
    expect(p.onOpenPrivacySettings).toHaveBeenCalledTimes(1);
    click(button(host, "Dismiss"));
    expect(p.onDismissSystemAudio).toHaveBeenCalledTimes(1);
  });

  it("Windows system audio copy has no System Settings button", () => {
    const { host } = render(<AppNotices {...props({ os: "windows", systemAudioBlocked: true })} />);
    expect(host.textContent).toContain("Windows isn’t sending any sound");
    expect(button(host, "Open System Settings")).toBeUndefined();
  });

  it("an app error is dismissible and keeps the full text under Details when briefed", () => {
    const long = `could not start capture: ${"device busy ".repeat(30)}`;
    const p = props({ appError: long });
    const { host } = render(<AppNotices {...p} />);
    expect(host.querySelector(".fly-notice-title")?.textContent).toBe("Something went wrong");
    expect(host.querySelector(".fly-notice-details-body")?.textContent).toBe(long);
    click(host.querySelector('[aria-label="Dismiss error"]'));
    expect(p.onDismissAppError).toHaveBeenCalledTimes(1);
  });

  it("a pipeline failure names the note and opens it", () => {
    const p = props({ pipelineFailures: [FAILURE] });
    const { host } = render(<AppNotices {...p} />);
    expect(host.textContent).toContain("“Weekly 1:1 with Dana” wasn’t transcribed.");
    // generic failure: no engine Settings shortcut
    expect(button(host, "Settings")).toBeUndefined();
    click(button(host, "Open note"));
    expect(p.onOpenFailedNote).toHaveBeenCalledWith(FAILURE);
    click(button(host, "Dismiss"));
    expect(p.onDismissPipelineFailure).toHaveBeenCalledWith("m2");
  });

  it("a first transcription failure offers Try again (it stays until the retry starts)", () => {
    const p = props({ pipelineFailures: [FAILURE] });
    const { host } = render(<AppNotices {...p} />);
    click(button(host, "Try again"));
    expect(p.onRetryFailure).toHaveBeenCalledWith(FAILURE);
    expect(button(host, "Open note")?.className).toContain("fly-btn--outline");
  });

  it("a failed re-run (transcript exists) has no Try again: it may have been a re-analysis", () => {
    const rerun = { ...FAILURE, hadTranscript: true };
    const { host } = render(<AppNotices {...props({ pipelineFailures: [rerun] })} />);
    expect(button(host, "Try again")).toBeUndefined();
    expect(button(host, "Open note")?.className).toContain("fly-btn--primary");
    expect(host.textContent).toContain("Its transcript is unchanged.");
    expect(host.textContent).not.toContain("wasn’t transcribed");
  });

  it("the stack is hidden in print (wrapper, since the primitive's display wins)", () => {
    const { host } = render(<AppNotices {...props({ appError: "x" })} />);
    expect(host.querySelector('[role="region"]')?.parentElement?.className).toBe("print:hidden");
  });

  it("an engine failure offers Settings; an unknown note has no Open note", () => {
    const failure = {
      ...FAILURE,
      noteId: null,
      noteTitle: null,
      error: "whisper-cli is not installed — install whisper.cpp",
    };
    const p = props({
      os: "macos",
      engine: { installed: false, managed: false },
      pipelineFailures: [failure],
    });
    const { host } = render(<AppNotices {...p} />);
    expect(host.querySelector(".fly-notice-title")?.textContent).toBe(
      "Transcription engine isn’t installed",
    );
    expect(host.textContent).toContain("brew install whisper-cpp");
    expect(button(host, "Open note")).toBeUndefined();
    click(button(host, "Settings"));
    expect(p.onOpenEngineSettings).toHaveBeenCalledTimes(1);
  });

  describe("update notice", () => {
    it("available: Update now / Later", () => {
      const updater = fakeUpdater("available");
      const { host } = render(<AppNotices {...props({ updater })} />);
      expect(host.querySelector(".fly-notice-title")?.textContent).toBe("Update available");
      click(button(host, "Update now"));
      expect(updater.downloadAndInstall).toHaveBeenCalledTimes(1);
      click(button(host, "Later"));
      expect(updater.dismiss).toHaveBeenCalledTimes(1);
    });

    it("downloading: a progress bar, no buttons", () => {
      const { host } = render(
        <AppNotices {...props({ updater: fakeUpdater("downloading", { progress: 0.42 }) })} />,
      );
      expect(host.querySelector('[role="progressbar"]')?.getAttribute("aria-valuenow")).toBe("42");
      expect(host.textContent).toContain("42%");
      // the bar announces progress; the changing text is hidden from AT
      const pctText = Array.from(host.querySelectorAll("span")).find(
        (s) => s.textContent === "42%",
      );
      expect(pctText?.getAttribute("aria-hidden")).toBe("true");
      expect(host.querySelectorAll(".fly-notice button")).toHaveLength(0);
    });

    it("ready: Restart now", () => {
      const updater = fakeUpdater("ready");
      const { host } = render(<AppNotices {...props({ updater })} />);
      click(button(host, "Restart now"));
      expect(updater.restart).toHaveBeenCalledTimes(1);
    });

    it("available says available, not ready", () => {
      const { host } = render(<AppNotices {...props({ updater: fakeUpdater("available") })} />);
      expect(host.textContent).toContain("Version 2.4.0 is available.");
    });

    it("a failed manual check (even after an earlier find) stays in Settings", () => {
      const updater = fakeUpdater("error", { error: "timeout", errorSource: "check" });
      const { host } = render(<AppNotices {...props({ updater })} />);
      expect(shownIds(host)).toEqual([]);
    });

    it("a failed download stays visible with Try again and the error under Details", () => {
      const updater = fakeUpdater("error", { error: "connection reset", errorSource: "install" });
      const { host } = render(<AppNotices {...props({ updater })} />);
      expect(host.querySelector(".fly-notice-title")?.textContent).toBe("Update didn’t install");
      expect(host.querySelector(".fly-notice-details-body")?.textContent).toBe("connection reset");
      click(button(host, "Try again"));
      expect(updater.downloadAndInstall).toHaveBeenCalledTimes(1);
    });

    it("hidden after Later", () => {
      const { host } = render(
        <AppNotices {...props({ updater: fakeUpdater("ready", { dismissed: true }) })} />,
      );
      expect(shownIds(host)).toEqual([]);
    });
  });
});
