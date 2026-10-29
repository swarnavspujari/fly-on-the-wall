import { describe, expect, it } from "vitest";
import {
  MACOS_BUNDLE_ID,
  NOTICE_PRIORITY,
  appErrorCopy,
  audioLostCopy,
  consentSkipsCopy,
  systemAudioCopy,
  updateCopy,
  updateNoticeVisible,
} from "./notices";

describe("NOTICE_PRIORITY", () => {
  it("orders the user's own failed action > system audio > pipeline errors > update", () => {
    expect(NOTICE_PRIORITY.appError).toBeGreaterThan(NOTICE_PRIORITY.systemAudio);
    expect(NOTICE_PRIORITY.systemAudio).toBeGreaterThan(NOTICE_PRIORITY.pipeline);
    expect(NOTICE_PRIORITY.pipeline).toBeGreaterThan(NOTICE_PRIORITY.update);
  });

  it("puts lost recording audio between system audio and pipeline errors", () => {
    expect(NOTICE_PRIORITY.systemAudio).toBeGreaterThan(NOTICE_PRIORITY.audioLost);
    expect(NOTICE_PRIORITY.audioLost).toBeGreaterThan(NOTICE_PRIORITY.pipeline);
  });

  it("puts skipped AI cleanup under pipeline errors and above the update", () => {
    expect(NOTICE_PRIORITY.pipeline).toBeGreaterThan(NOTICE_PRIORITY.consentSkips);
    expect(NOTICE_PRIORITY.consentSkips).toBeGreaterThan(NOTICE_PRIORITY.update);
  });
});

describe("consentSkipsCopy", () => {
  it("names who needs the OK and what each meeting is missing", () => {
    const c = consentSkipsCopy(3, "Anthropic");
    expect(c.title).toBe("Transcript cleanup and action items were skipped");
    expect(c.cause).toBe(
      "Anthropic needs your OK before it gets meeting content, so 3 meetings kept the raw transcript and got no action items.",
    );
    expect(c.action).toBe("Review and allow");
  });

  it("a missing API key comes first and points to Settings", () => {
    const c = consentSkipsCopy(2, "OpenAI", "OpenAI");
    expect(c.cause).toBe(
      "There's no API key for OpenAI yet, so 2 meetings kept the raw transcript and got no action items. Add one in Settings › Ask and Enhance, then run them.",
    );
    expect(c.action).toBe("Add a key");
  });

  it("once nobody needs to allow anything, it only offers to run them", () => {
    const c = consentSkipsCopy(1, null);
    expect(c.cause).toBe(
      "1 meeting kept the raw transcript and got no action items. You can run them now.",
    );
    expect(c.action).toBe("Run them now");
  });
});

describe("systemAudioCopy", () => {
  it("macOS: the stale-grant fix, with tccutil under details", () => {
    const c = systemAudioCopy("macos");
    expect(c.cause).toContain("macOS is blocking system audio");
    expect(c.steps).toHaveLength(3);
    expect(c.steps[0]).toContain("Screen & System Audio Recording");
    expect(c.steps[1]).toContain("remove it with −");
    expect(c.details).toContain(`tccutil reset ScreenCapture ${MACOS_BUNDLE_ID}`);
    expect(c.detailsLabel).toBe("Still blocked?");
  });

  it("Windows: muted output and hands-free headsets, no macOS wording", () => {
    const c = systemAudioCopy("windows");
    expect(c.cause).toContain("Windows isn’t sending any sound");
    expect(c.steps.join(" ")).toMatch(/unmute/i);
    expect(c.steps.join(" ")).toMatch(/hands-free/);
    expect(JSON.stringify(c)).not.toMatch(/macOS|tccutil|System Settings/);
  });

  it("other systems get the same fix without naming Windows", () => {
    const c = systemAudioCopy("linux");
    expect(c.cause.startsWith("Your computer")).toBe(true);
    expect(JSON.stringify(c)).not.toMatch(/Windows|macOS/);
  });

  it("every variant leads with the consequence, in sentence case", () => {
    for (const os of ["macos", "windows", "linux", null]) {
      const c = systemAudioCopy(os);
      expect(c.title).toBe("Can’t hear the other participants");
      expect(c.cause).toContain("only capture your microphone");
    }
  });
});

describe("appErrorCopy", () => {
  it("short errors show whole, with no details", () => {
    const c = appErrorCopy("Microphone is busy");
    expect(c.cause).toBe("Microphone is busy");
    expect(c.details).toBeNull();
    expect(c.steps).toEqual([]);
  });

  it("long or URL-laden errors are briefed; the full text stays under details", () => {
    const raw = `connect failed: https://oauth.example.com/${"x".repeat(80)} ${"y".repeat(200)}`;
    const c = appErrorCopy(raw);
    expect(c.cause).not.toMatch(/https?:/);
    expect(c.cause.length).toBeLessThanOrEqual(161);
    expect(c.details).toBe(raw);
  });
});

describe("updateNoticeVisible", () => {
  const base = { dismissed: false };

  it("shows from available through installing", () => {
    for (const phase of ["available", "downloading", "ready", "installing"] as const) {
      expect(updateNoticeVisible({ ...base, phase })).toBe(true);
    }
  });

  it("hides when idle, checking or up to date", () => {
    for (const phase of ["idle", "checking", "upToDate"] as const) {
      expect(updateNoticeVisible({ ...base, phase })).toBe(false);
    }
  });

  it("Later hides it for the session", () => {
    expect(updateNoticeVisible({ ...base, phase: "ready", dismissed: true })).toBe(false);
  });

  it("an error shows only for a failed download/install (a failed check belongs to Settings)", () => {
    expect(updateNoticeVisible({ ...base, phase: "error", errorSource: "install" })).toBe(true);
    // a manual "Check for updates" failing after an earlier find
    expect(updateNoticeVisible({ ...base, phase: "error", errorSource: "check" })).toBe(false);
    expect(updateNoticeVisible({ ...base, phase: "error" })).toBe(false);
  });
});

describe("updateCopy", () => {
  it("names the version and what happens next", () => {
    expect(updateCopy("available", "2.4.0", null).cause).toBe(
      "Version 2.4.0 is available. Fly on the Wall restarts once to apply it.",
    );
    expect(updateCopy("ready", "2.4.0", null).title).toBe("Update ready");
    expect(updateCopy("ready", "2.4.0", null).cause).toBe(
      "Version 2.4.0 installs when you restart.",
    );
  });

  it("an error keeps the raw text for details", () => {
    const c = updateCopy("error", "2.4.0", "signature mismatch");
    expect(c.title).toBe("Update didn’t install");
    expect(c.details).toBe("signature mismatch");
  });
});

describe("audioLostCopy", () => {
  const lost = (mic: number, sys: number) => ({
    meeting_id: "m1",
    note_id: "n1",
    mic_lost_ms: mic,
    system_lost_ms: sys,
  });

  it("names each side that lost audio, in seconds", () => {
    expect(audioLostCopy(lost(1250, 0)).cause).toBe(
      "1.3 s of your microphone was lost because the computer was too busy. Those moments are silent in the recording and missing from its transcript.",
    );
    expect(audioLostCopy(lost(0, 60)).cause).toMatch(
      /^0.1 s of the other participants’ audio was lost/,
    );
    expect(audioLostCopy(lost(900, 2000)).cause).toMatch(
      /^0.9 s of your microphone and 2.0 s of the other participants’ audio were lost/,
    );
  });

  it("says what to do next time", () => {
    const c = audioLostCopy(lost(500, 0));
    expect(c.title).toBe("Part of the last recording is missing");
    expect(c.steps).toEqual(["Before your next meeting, close apps that keep the computer busy."]);
  });
});
