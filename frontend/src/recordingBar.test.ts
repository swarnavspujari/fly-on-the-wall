import { describe, expect, it } from "vitest";
import { classifyWarning, healthChips, remindNoticeOn, REMIND_NOTICE_KEY } from "./recordingBar";

// The exact sentences the backend sends today (fly-audio cpal_backend.rs,
// win_volume, tap_messages.rs). The chip labels key off these.
const MUTED =
  "System output is muted — the other participants' audio will record as silence. Unmute your speakers.";
const VOLUME_ZERO =
  "System volume is at 0% — the other participants' audio will record as silence. Raise your system volume.";
const LOOPBACK =
  "System audio capture couldn't start (device busy) — only your microphone is being recorded, so the other participants won't be in this recording.";
const MACOS =
  "System audio is playing but its capture is recording only silence — macOS is blocking system-audio capture for this build, so the other participants may be missing from this recording. Open System Settings → Privacy & Security → Screen & System Audio Recording and allow Fly on the Wall.";
// lost_audio_warning (cpal_backend.rs), pinned there by
// lost_audio_warning_matches_the_recording_bar_classifier
const MIC_LOST =
  "Your microphone has gaps: 1.2 s was lost because the computer was too busy (most recently at 12:34). The rest is still recording — closing heavy apps can help.";
const SYSTEM_LOST =
  "The other participants' audio has gaps: 0.4 s was lost because the computer was too busy (most recently at 3:05). The rest is still recording — closing heavy apps can help.";

describe("classifyWarning", () => {
  it("puts lost microphone audio on the you chip", () => {
    expect(classifyWarning(MIC_LOST)).toMatchObject({ side: "you", label: "Audio lost" });
  });

  it("puts lost system audio on the others chip", () => {
    expect(classifyWarning(SYSTEM_LOST)).toMatchObject({ side: "others", label: "Audio lost" });
  });

  it("labels a muted output on the others chip", () => {
    expect(classifyWarning(MUTED)).toMatchObject({
      side: "others",
      label: "Output muted",
      privacyPane: false,
    });
  });

  it("labels a 0% system volume on the others chip", () => {
    expect(classifyWarning(VOLUME_ZERO)).toMatchObject({ side: "others", label: "Volume at 0%" });
  });

  it("labels a loopback that never started on the others chip, not the mic", () => {
    // "only your microphone" must not be read as a microphone problem
    expect(classifyWarning(LOOPBACK)).toMatchObject({ side: "others", label: "Not recorded" });
  });

  it("labels the macOS silence warning and offers the privacy pane", () => {
    expect(classifyWarning(MACOS)).toMatchObject({
      side: "others",
      label: "Blocked by macOS",
      privacyPane: true,
    });
  });

  it("puts an unknown microphone warning on the you chip", () => {
    expect(classifyWarning("The microphone stopped sending audio.")).toMatchObject({
      side: "you",
      label: "Check your mic",
    });
  });

  it("puts an unknown, non-mic warning on the others chip as 'Check audio'", () => {
    expect(classifyWarning("Something about the loopback looks odd.")).toMatchObject({
      side: "others",
      label: "Check audio",
    });
  });

  it("keeps the full sentence for the popover", () => {
    expect(classifyWarning(MUTED).text).toBe(MUTED);
  });
});

describe("healthChips", () => {
  it("never claims the mic is fine: nothing monitors it yet", () => {
    const chips = healthChips([], false);
    expect(chips.map((c) => [c.side, c.tone, c.label])).toEqual([
      ["you", "unmonitored", "You"],
      ["others", "ok", "Others"],
    ]);
  });

  it("flags only the side a warning belongs to", () => {
    const [you, others] = healthChips([MUTED], false);
    expect(you.tone).toBe("unmonitored");
    expect(others.tone).toBe("warn");
    expect(others.label).toBe("Others · Output muted");
    expect(others.warnings.map((w) => w.text)).toEqual([MUTED]);
  });

  it("names the first warning and counts the rest", () => {
    const [, others] = healthChips([LOOPBACK, VOLUME_ZERO], false);
    expect(others.label).toBe("Others · Not recorded +1");
    expect(others.warnings).toHaveLength(2);
  });

  it("goes neutral while paused, but keeps warnings reachable", () => {
    const [you, others] = healthChips([MUTED], true);
    expect(you.tone).toBe("paused");
    expect(others.tone).toBe("warn");
  });
});

describe("remind setting", () => {
  it("uses a webview-settable ui. key", () => {
    expect(REMIND_NOTICE_KEY.startsWith("ui.")).toBe(true);
  });

  it("defaults on when unset and turns off only on an explicit false", () => {
    expect(remindNoticeOn(null)).toBe(true);
    expect(remindNoticeOn(undefined)).toBe(true);
    expect(remindNoticeOn("true")).toBe(true);
    expect(remindNoticeOn("false")).toBe(false);
  });
});
