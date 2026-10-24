import { describe, expect, it } from "vitest";
import { isSafeExternalHref, isSafeMediaSrc } from "./safeContent";

describe("isSafeMediaSrc", () => {
  it("allows only sources that never leave the machine", () => {
    expect(isSafeMediaSrc("data:image/png;base64,AAAA")).toBe(true);
    expect(isSafeMediaSrc("http://asset.localhost/C%3A/data/recordings/a.wav")).toBe(true);
    expect(isSafeMediaSrc("asset://localhost/%2Fdata%2Fx.jpg")).toBe(true);
    expect(isSafeMediaSrc("blob:http://tauri.localhost/1234")).toBe(true);
  });

  it("blocks remote and tricky sources (exfiltration beacons)", () => {
    for (const bad of [
      "https://attacker.example/x.png?d=secret",
      "http://attacker.example/x.png",
      "//attacker.example/x.png",
      "http://asset.localhost.attacker.example/x.png",
      "data:image/svg+xml;base64,PHN2Zz4=",
      "data:text/html,<script>alert(1)</script>",
      "javascript:alert(1)",
      "",
      null,
      undefined,
    ]) {
      expect(isSafeMediaSrc(bad as string)).toBe(false);
    }
  });
});

describe("isSafeExternalHref", () => {
  it("allows web and mail links", () => {
    expect(isSafeExternalHref("https://example.com/a?b=1")).toBe(true);
    expect(isSafeExternalHref("http://example.com")).toBe(true);
    expect(isSafeExternalHref("mailto:a@example.com")).toBe(true);
  });

  it("refuses schemes that can launch programs or run script", () => {
    for (const bad of [
      "javascript:alert(1)",
      "file:///C:/Windows/System32/calc.exe",
      "ms-settings:privacy",
      "data:text/html,hi",
      "relative/path",
      "",
      undefined,
    ]) {
      expect(isSafeExternalHref(bad)).toBe(false);
    }
  });
});
