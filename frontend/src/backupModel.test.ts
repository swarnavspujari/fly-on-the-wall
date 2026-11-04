import { describe, expect, it } from "vitest";
import { BACKUPS_KEPT, backupNoticeCopy, backupRowHelper } from "./backupModel";

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

  it("the row says how many are kept and that recordings aren't copied", () => {
    expect(backupRowHelper(0)).toContain("saves a copy of it here");
    expect(backupRowHelper(2)).toContain(`The newest ${BACKUPS_KEPT} are kept`);
    for (const n of [0, 2, 3]) expect(backupRowHelper(n)).toContain("Recordings aren’t copied");
  });
});
