import { describe, expect, it } from "vitest";
import {
  BACKUPS_KEPT,
  BACKUP_HINT_AFTER_S,
  backupNoticeCopy,
  backupRowHelper,
  backupStepHint,
} from "./backupModel";

describe("backup copy", () => {
  it("names disk space only when that was the reason", () => {
    const space = backupNoticeCopy("space");
    expect(space.cause).toContain("before upgrading: not enough disk space");
    const failed = backupNoticeCopy("failed");
    expect(failed.cause).toContain("couldn’t save a backup of your library before upgrading");
    expect(failed.cause).not.toContain("disk space");
    for (const c of [space, failed]) {
      expect(c.title).toBe("No backup of your library was saved");
      // the library was upgraded anyway: say what that means
      expect(c.cause).toContain("upgraded without a copy");
      expect(c.steps.length).toBeGreaterThan(0);
    }
  });

  it("a failed backup before compression doesn't claim anything was upgraded", () => {
    const space = backupNoticeCopy("compression_space");
    const failed = backupNoticeCopy("compression_failed");
    expect(space.cause).toContain(
      "before storing your recordings more compactly: not enough disk space",
    );
    expect(failed.cause).toContain("before storing your recordings more compactly.");
    expect(failed.cause).not.toContain("disk space");
    for (const c of [space, failed]) {
      expect(c.title).toBe("No backup of your library was saved");
      expect(c.cause).not.toMatch(/upgrad/i);
      expect(c.cause).toContain("checked sample for sample");
      expect(c.steps.length).toBeGreaterThan(0);
    }
    // it is tried again next launch, unlike the update-time copy
    expect(space.steps.join(" ")).toContain("tries again the next time it starts");
  });

  it("the row says how many are kept and that recordings aren't copied", () => {
    expect(backupRowHelper(0)).toContain("saves a copy of it here");
    expect(backupRowHelper(2)).toContain(`The newest ${BACKUPS_KEPT} are kept`);
    // the pre-change copy outlives the newest ones, so deleted notes can too
    expect(backupRowHelper(2)).toContain("plus the last copy from before each recent change");
    expect(backupRowHelper(2)).toContain("stays in older copies until they’re replaced");
    for (const n of [0, 2, 3]) expect(backupRowHelper(n)).toContain("Recordings aren’t copied");
  });

  it("the startup step says nothing for a quick copy, then the size and the time so far", () => {
    expect(backupStepHint(1_500_000_000, 0)).toBeNull();
    expect(backupStepHint(1_500_000_000, BACKUP_HINT_AFTER_S - 1)).toBeNull();
    expect(backupStepHint(1_500_000_000, BACKUP_HINT_AFTER_S)).toBe("Copying 1.4 GB. 3 s so far.");
    expect(backupStepHint(52_428_800, 12)).toBe("Copying 50 MB. 12 s so far.");
    expect(backupStepHint(1_500_000_000, 75)).toBe("Copying 1.4 GB. 1 min 15 s so far.");
    // no size reported (an older backend): just the time
    expect(backupStepHint(0, 5)).toBe("5 s so far.");
  });
});
