// Copies the release flyonthewall-mcp binary into src-tauri/binaries with the
// target-triple suffix Tauri's externalBin bundling expects.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";

const host = execFileSync("rustc", ["-vV"])
  .toString()
  .split("\n")
  .find((l) => l.startsWith("host:"))
  .split(":")[1]
  .trim();

const ext = process.platform === "win32" ? ".exe" : "";
const src = join("target", "release", `flyonthewall-mcp${ext}`);
const destDir = join("src-tauri", "binaries");
mkdirSync(destDir, { recursive: true });
const dest = join(destDir, `flyonthewall-mcp-${host}${ext}`);
copyFileSync(src, dest);
console.log(`sidecar ready: ${dest}`);
