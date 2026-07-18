// Download/usage stats collector — runs daily via .github/workflows/stats.yml.
//
// Reads GitHub release + traffic data and writes three artifacts under docs/data/
// (served publicly by GitHub Pages):
//   stats.json            — current headline numbers (the landing page fetches this)
//   downloads-badge.json  — Shields "endpoint" badge payload (the README badge reads this)
//   stats-history.jsonl   — one raw per-asset snapshot appended per run (the long-term log)
//
// Counting rules:
//   - Only app installer assets count as downloads (.exe/.msi/.dmg/.AppImage/.deb).
//     latest.json (updater polls) and *.sig are tracked separately; tools-* releases
//     (whisper engine artifacts) are excluded entirely.
//   - totalDownloads    = sum of installer downloads across all releases. Exact, but
//     includes auto-updater re-downloads, so it overstates unique users.
//   - estimatedInstalls = per-installer-flavor maximum across releases, summed.
//     Rationale: every active install downloads each release at most once, so the
//     busiest single release per flavor approximates the concurrent install base.
//     A conservative lower bound on unique installs.
//   - updateChecks      = sum of latest.json downloads (an app-launch/update-poll proxy).

import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";

const REPO = process.env.GITHUB_REPOSITORY ?? "swarnavspujari/fly-on-the-wall";
const TOKEN = process.env.GITHUB_TOKEN;
if (!TOKEN) {
  console.error("GITHUB_TOKEN is required");
  process.exit(1);
}

async function api(path) {
  const res = await fetch(`https://api.github.com${path}`, {
    headers: {
      authorization: `Bearer ${TOKEN}`,
      accept: "application/vnd.github+json",
      "x-github-api-version": "2022-11-28",
    },
  });
  if (!res.ok) {
    const wants = res.headers.get("x-accepted-github-permissions");
    throw new Error(`${path} -> ${res.status}${wants ? ` (accepted permissions: ${wants})` : ""}`);
  }
  return res.json();
}

const flavor = (name) => {
  const n = name.toLowerCase();
  if (n.endsWith(".sig") || n === "latest.json") return null;
  if (n.endsWith(".exe") || n.endsWith(".msi")) return "windows-x64";
  if (n.endsWith(".dmg")) return n.includes("aarch64") || n.includes("arm64") ? "macos-arm64" : "macos-x64";
  if (n.endsWith(".appimage")) return "linux-appimage";
  if (n.endsWith(".deb")) return "linux-deb";
  return null;
};
const PLATFORM_OF = { "windows-x64": "windows", "macos-arm64": "macos", "macos-x64": "macos", "linux-appimage": "linux", "linux-deb": "linux" };

const releases = (await api(`/repos/${REPO}/releases?per_page=100`)).filter(
  (r) => !r.draft && !r.tag_name.startsWith("tools-"),
);

let totalDownloads = 0;
let updateChecks = 0;
const perPlatform = { windows: 0, macos: 0, linux: 0 };
const flavorMax = {};
const snapshotReleases = [];

for (const r of releases) {
  const assets = [];
  for (const a of r.assets) {
    assets.push({ name: a.name, count: a.download_count });
    if (a.name === "latest.json") { updateChecks += a.download_count; continue; }
    const f = flavor(a.name);
    if (!f) continue;
    totalDownloads += a.download_count;
    perPlatform[PLATFORM_OF[f]] += a.download_count;
    flavorMax[f] = Math.max(flavorMax[f] ?? 0, a.download_count);
  }
  snapshotReleases.push({ tag: r.tag_name, assets });
}

const estimatedInstalls = Object.values(flavorMax).reduce((a, b) => a + b, 0);
const latest = releases.find((r) => !r.prerelease);
const latestDownloads = latest
  ? latest.assets.reduce((sum, a) => sum + (flavor(a.name) ? a.download_count : 0), 0)
  : 0;

// Repo traffic (needs push access; absent for forks/restricted tokens — degrade to null).
let traffic = null;
try {
  const [views, clones] = await Promise.all([
    api(`/repos/${REPO}/traffic/views`),
    api(`/repos/${REPO}/traffic/clones`),
  ]);
  traffic = { views14d: views.count, viewers14d: views.uniques, clones14d: clones.count, cloners14d: clones.uniques };
} catch (e) {
  console.warn(`traffic API unavailable (${e.message}) — skipping`);
}

const now = new Date().toISOString();
mkdirSync("docs/data", { recursive: true });

writeFileSync(
  "docs/data/stats.json",
  JSON.stringify(
    {
      updated: now,
      totalDownloads,
      estimatedInstalls,
      perPlatform,
      updateChecks,
      latestRelease: latest ? { tag: latest.tag_name, downloads: latestDownloads } : null,
      note: "totalDownloads = installer downloads across all releases (includes auto-updater re-downloads); estimatedInstalls = per-flavor max across releases, a conservative unique-install floor.",
    },
    null,
    2,
  ) + "\n",
);

writeFileSync(
  "docs/data/downloads-badge.json",
  JSON.stringify({ schemaVersion: 1, label: "downloads", message: String(totalDownloads), color: "6a4ae0" }) + "\n",
);

appendFileSync(
  "docs/data/stats-history.jsonl",
  JSON.stringify({ date: now, totalDownloads, estimatedInstalls, updateChecks, traffic, releases: snapshotReleases }) + "\n",
);

console.log(`downloads=${totalDownloads} estimatedInstalls=${estimatedInstalls} updateChecks=${updateChecks} traffic=${JSON.stringify(traffic)}`);
