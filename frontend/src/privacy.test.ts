import { describe, expect, it } from "vitest";
import {
  canConfirmWipe,
  encryptionBannerText,
  formatBytes,
  isKeptLocal,
  keepLocalTitle,
  parseRetention,
  recordingNoticeText,
  retentionOptions,
  retentionWarning,
  showEncryptionBanner,
} from "./privacy";
import type { MeetingPrivacy } from "./types";

const privacy = (p: Partial<MeetingPrivacy>): MeetingPrivacy => ({
  meeting_id: "m1",
  keep_local: false,
  keep_local_from_folder: false,
  participants_notified: false,
  audio_deleted_at: null,
  ...p,
});

describe("recording notice", () => {
  it("names the meeting when it has a title", () => {
    const t = recordingNoticeText("Acme renewal");
    expect(t).toContain('"Acme renewal"');
    expect(t).toMatch(/recording/i);
    expect(t).toMatch(/rather I didn't record/);
  });
  it("reads naturally without a title", () => {
    expect(recordingNoticeText("  ")).toContain("recording this meeting to take notes");
    expect(recordingNoticeText(null)).not.toContain('""');
  });
  it("fits the backend's clipboard limit", () => {
    expect(recordingNoticeText("x".repeat(500)).length).toBeLessThanOrEqual(2000);
  });
});

describe("retention", () => {
  const choices = [365, 7, 90, 30];
  it("lists Never first, then the choices in order", () => {
    expect(retentionOptions(choices).map((o) => o.value)).toEqual(["", "7", "30", "90", "365"]);
    expect(retentionOptions(choices)[4].label).toBe("After 1 year");
  });
  it("parses only offered values", () => {
    expect(parseRetention("", choices)).toBeNull();
    expect(parseRetention("30", choices)).toBe(30);
    expect(parseRetention("1", choices)).toBeNull();
    expect(parseRetention("abc", choices)).toBeNull();
  });
  it("warns only when something would be deleted now", () => {
    expect(retentionWarning(30, 0)).toBeNull();
    expect(retentionWarning(30, 1)).toContain("1 meeting older than 30 days");
    expect(retentionWarning(365, 4)).toContain("4 meetings older than 1 year");
    expect(retentionWarning(7, 2)).toContain("Transcripts and notes are kept");
  });
});

describe("disk encryption banner", () => {
  const off = { state: "off" as const, method: "BitLocker" as const, volume: "C:" };
  it("shows only for a clear off that wasn't dismissed", () => {
    expect(showEncryptionBanner(off, null)).toBe(true);
    expect(showEncryptionBanner(off, "true")).toBe(false);
    expect(showEncryptionBanner({ ...off, state: "unknown" }, null)).toBe(false);
    expect(showEncryptionBanner({ ...off, state: "on" }, null)).toBe(false);
    expect(showEncryptionBanner(null, null)).toBe(false);
  });
  it("explains the risk with the method and drive", () => {
    const t = encryptionBannerText(off);
    expect(t).toContain("BitLocker");
    expect(t).toContain("for the C: drive.");
    expect(t).toMatch(/only as protected as the disk/);
    expect(encryptionBannerText({ state: "off", method: "FileVault", volume: null })).not.toContain(
      "drive",
    );
  });
});

describe("delete all my data", () => {
  it("needs both the checkbox and the typed phrase", () => {
    expect(canConfirmWipe(false, "delete everything")).toBe(false);
    expect(canConfirmWipe(true, "delete")).toBe(false);
    expect(canConfirmWipe(true, "  Delete Everything ")).toBe(true);
  });
});

describe("keep local", () => {
  it("is effective from the note or its folder", () => {
    expect(isKeptLocal(null)).toBe(false);
    expect(isKeptLocal(privacy({}))).toBe(false);
    expect(isKeptLocal(privacy({ keep_local: true }))).toBe(true);
    expect(isKeptLocal(privacy({ keep_local_from_folder: true }))).toBe(true);
  });
  it("explains where the setting comes from", () => {
    expect(keepLocalTitle(privacy({ keep_local_from_folder: true }))).toMatch(/folder/);
    expect(keepLocalTitle(privacy({ keep_local: true }))).toMatch(/Ollama/);
    expect(keepLocalTitle(privacy({}))).toMatch(/never send/);
  });
});

it("formats byte counts", () => {
  expect(formatBytes(512)).toBe("512 B");
  expect(formatBytes(1536)).toBe("1.5 KB");
  expect(formatBytes(250 * 1024 * 1024)).toBe("250 MB");
});
