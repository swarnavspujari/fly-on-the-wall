// @vitest-environment jsdom
import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import { ConfirmProvider, useConfirm, type ConfirmFn } from "./confirm";
import { confirmChoice, confirmDialog, confirmTitle } from "./confirmTesting";
import { Modal } from "./ui";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;
let ask: ConfirmFn;

function Grab() {
  const confirm = useConfirm();
  useEffect(() => {
    ask = confirm;
  }, [confirm]);
  return <button type="button">Trigger</button>;
}

function render(ui: React.ReactNode = <Grab />) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<ConfirmProvider>{ui}</ConfirmProvider>));
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
});

const flush = async () => {
  for (let i = 0; i < 3; i++) await act(async () => {});
};

/** Ask, and return a probe for the answer (undefined until it arrives). */
async function question(opts: Parameters<ConfirmFn>[0]) {
  let result: boolean | undefined;
  await act(async () => {
    void ask(opts).then((ok) => (result = ok));
  });
  return () => result;
}

const escape = () =>
  act(() => {
    document.activeElement!.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
  });

const DELETE = {
  title: "Delete folder “Q3”?",
  confirmLabel: "Delete folder",
  tone: "danger",
} as const;

describe("useConfirm", () => {
  it("waits for an explicit yes", async () => {
    render();
    const answer = await question({ title: "Keep local?", confirmLabel: "Keep local" });
    expect(confirmTitle()).toBe("Keep local?");
    expect(answer()).toBeUndefined(); // nothing is decided while it's open
    await act(async () => confirmChoice(true).click());
    expect(answer()).toBe(true);
    expect(confirmDialog()).toBeNull();
  });

  it("Cancel, Esc, the backdrop and the close button all mean no", async () => {
    render();
    let answer = await question(DELETE);
    await act(async () => confirmChoice(false).click());
    expect(answer()).toBe(false);

    answer = await question(DELETE);
    escape();
    await flush();
    expect(answer()).toBe(false);
    expect(confirmDialog()).toBeNull();

    answer = await question(DELETE);
    await act(async () => document.querySelector<HTMLElement>(".fly-modal-overlay")!.click());
    expect(answer()).toBe(false);

    answer = await question(DELETE);
    await act(async () => document.querySelector<HTMLElement>("[data-dialog-close]")!.click());
    expect(answer()).toBe(false);
  });

  it("starts on Cancel, so Enter never says yes by accident", async () => {
    render();
    await question({ title: "Use OpenAI for Ask and Enhance?", confirmLabel: "Use OpenAI" });
    expect(document.activeElement).toBe(confirmChoice(false));
    expect(confirmDialog()!.getAttribute("role")).toBe("alertdialog");
    expect(confirmDialog()!.getAttribute("aria-modal")).toBe("true");
  });

  it("danger: the red action sits apart, on the far side from Cancel", async () => {
    render();
    await question({ ...DELETE, message: "Notes inside become unfiled." });
    const foot = [...confirmDialog()!.querySelectorAll(".fly-modal-foot button")];
    expect(foot.map((b) => b.textContent)).toEqual(["Delete folder", "Cancel"]);
    expect(confirmDialog()!.dataset.tone).toBe("danger");
    expect((foot[0] as HTMLElement).style.color).toBe("var(--error-text)");
    expect(foot[0].className).toContain("mr-auto");
    // the message describes the dialog for screen readers
    const describedBy = confirmDialog()!.getAttribute("aria-describedby")!;
    expect(document.getElementById(describedBy)!.textContent).toBe("Notes inside become unfiled.");
  });

  it("a plain question reads Cancel, then the action", async () => {
    render();
    await question({ title: "Keep local?", confirmLabel: "Keep local", cancelLabel: "Not now" });
    const foot = [...confirmDialog()!.querySelectorAll(".fly-modal-foot button")];
    expect(foot.map((b) => b.textContent)).toEqual(["Not now", "Keep local"]);
    expect(foot[1].className).toContain("fly-btn--primary");
  });

  it("splits a string message into paragraphs at blank lines", async () => {
    render();
    await question({ title: "T?", confirmLabel: "Go", message: "First.\n\nSecond." });
    const paras = [...confirmDialog()!.querySelectorAll("p")].map((p) => p.textContent);
    expect(paras).toEqual(["First.", "Second."]);
  });

  it("asks one question at a time, in order", async () => {
    render();
    const first = await question({ title: "First?", confirmLabel: "Yes" });
    const second = await question({ title: "Second?", confirmLabel: "Yes" });
    expect(document.querySelectorAll('[data-testid="confirm-dialog"]')).toHaveLength(1);
    expect(confirmTitle()).toBe("First?");
    await act(async () => confirmChoice(false).click());
    await flush();
    expect(confirmTitle()).toBe("Second?");
    await act(async () => confirmChoice(true).click());
    expect([first(), second()]).toEqual([false, true]);
  });

  it("Esc answers the question before closing a dialog it was asked from", async () => {
    let closed = 0;
    render(
      <Modal open title="Settings" onClose={() => (closed += 1)}>
        <Grab />
      </Modal>,
    );
    const answer = await question(DELETE);
    escape();
    await flush();
    expect(answer()).toBe(false);
    expect(closed).toBe(0);
    escape();
    expect(closed).toBe(1);
  });

  it("answers no to anything still open when it unmounts", async () => {
    render();
    const answer = await question(DELETE);
    act(() => root!.unmount());
    root = null;
    await flush();
    expect(answer()).toBe(false);
  });

  it("needs a ConfirmProvider", () => {
    host = document.createElement("div");
    root = createRoot(host);
    const errors: unknown[] = [];
    const onError = (e: ErrorEvent) => {
      errors.push(e.error);
      e.preventDefault();
    };
    window.addEventListener("error", onError);
    try {
      expect(() => act(() => root!.render(<Grab />))).toThrow(/ConfirmProvider/);
    } finally {
      window.removeEventListener("error", onError);
    }
  });
});
