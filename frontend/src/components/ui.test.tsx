// @vitest-environment jsdom
import { act, useState } from "react";
import type { ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  Modal,
  Notice,
  NoticeStack,
  Segmented,
  SettingsGroup,
  SettingsRow,
  Sheet,
  Switch,
  escapeLayerDepth,
  orderNotices,
  pushEscapeLayer,
  useEscapeLayer,
} from "./ui";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let mounted: { root: Root; host: HTMLElement }[] = [];

function render(ui: ReactNode) {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  act(() => root.render(ui));
  mounted.push({ root, host });
  return {
    host,
    rerender: (next: ReactNode) => act(() => root.render(next)),
    unmount: () => act(() => root.unmount()),
  };
}

afterEach(() => {
  for (const { root, host } of mounted) {
    act(() => root.unmount());
    host.remove();
  }
  mounted = [];
});

const pressEscape = (target: EventTarget = document.body) => {
  const e = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
  act(() => {
    target.dispatchEvent(e);
  });
  return e;
};

const click = (el: Element | null) => {
  if (!el) throw new Error("element not found");
  act(() => {
    (el as HTMLElement).click();
  });
};

describe("Switch", () => {
  it("exposes its state with aria-pressed and its name with aria-label", () => {
    const { host } = render(<Switch checked={false} label="Transcribe when I stop" />);
    const btn = host.querySelector("button")!;
    expect(btn.getAttribute("aria-pressed")).toBe("false");
    expect(btn.getAttribute("aria-label")).toBe("Transcribe when I stop");
    expect(btn.getAttribute("type")).toBe("button");
  });

  it("reports the next state on click", () => {
    function Harness() {
      const [on, setOn] = useState(false);
      return <Switch checked={on} onChange={setOn} label="Remind me" />;
    }
    const { host } = render(<Harness />);
    const btn = host.querySelector("button")!;
    click(btn);
    expect(btn.getAttribute("aria-pressed")).toBe("true");
    click(btn);
    expect(btn.getAttribute("aria-pressed")).toBe("false");
  });

  it("does nothing while disabled", () => {
    const onChange = vi.fn();
    const { host } = render(
      <Switch checked label="Use the graphics card" disabled onChange={onChange} />,
    );
    click(host.querySelector("button"));
    expect(onChange).not.toHaveBeenCalled();
  });
});

describe("Segmented", () => {
  const options = [
    { value: "light", label: "Fast" },
    { value: "balanced", label: "Balanced" },
    { value: "best", label: "Best" },
  ];

  it("renders a labelled radio group with the selection checked", () => {
    const { host } = render(
      <Segmented label="Quality" value="balanced" onChange={() => {}} options={options} />,
    );
    const group = host.querySelector('[role="radiogroup"]')!;
    expect(group.getAttribute("aria-label")).toBe("Quality");
    const radios = [...host.querySelectorAll('[role="radio"]')];
    expect(radios.map((r) => r.getAttribute("aria-checked"))).toEqual(["false", "true", "false"]);
    // roving tab stop: only the selected segment is tabbable
    expect(radios.map((r) => r.getAttribute("tabindex"))).toEqual(["-1", "0", "-1"]);
  });

  it("selects on click and with arrow keys, skipping disabled options", () => {
    function Harness() {
      const [v, setV] = useState("light");
      return (
        <Segmented
          label="Quality"
          value={v}
          onChange={setV}
          options={[options[0], { ...options[1], disabled: true }, options[2]]}
        />
      );
    }
    const { host } = render(<Harness />);
    const group = host.querySelector('[role="radiogroup"]')!;
    const checked = () => host.querySelector('[aria-checked="true"]')?.textContent;
    act(() => {
      group.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    });
    expect(checked()).toBe("Best");
    act(() => {
      group.dispatchEvent(new KeyboardEvent("keydown", { key: "Home", bubbles: true }));
    });
    expect(checked()).toBe("Fast");
    click(host.querySelectorAll('[role="radio"]')[2]);
    expect(checked()).toBe("Best");
  });

  it("does not re-fire onChange for the current value", () => {
    const onChange = vi.fn();
    const { host } = render(
      <Segmented label="Quality" value="best" onChange={onChange} options={options} />,
    );
    click(host.querySelectorAll('[role="radio"]')[2]);
    expect(onChange).not.toHaveBeenCalled();
  });
});

describe("SettingsGroup / SettingsRow", () => {
  it("renders title, helper, badge and control in one row", () => {
    const { host } = render(
      <SettingsGroup>
        <SettingsRow
          title="Made on"
          helper="Audio is sent to Groq."
          badge={<span data-testid="badge">Audio leaves this computer</span>}
          control={<button>pick</button>}
        >
          <input aria-label="Groq API key" />
        </SettingsRow>
      </SettingsGroup>,
    );
    expect(host.querySelector(".fly-srow-title")?.textContent).toBe(
      "Made onAudio leaves this computer",
    );
    expect(host.querySelector(".fly-srow-helper")?.textContent).toBe("Audio is sent to Groq.");
    expect(host.querySelector(".fly-srow-control button")?.textContent).toBe("pick");
    expect(host.querySelector(".fly-srow-below input")).not.toBeNull();
  });

  it("hides advanced rows behind More options and toggles the label", () => {
    const { host } = render(
      <SettingsGroup advanced={<SettingsRow title="Model" />}>
        <SettingsRow title="Quality" />
      </SettingsGroup>,
    );
    const more = host.querySelector(".fly-sgroup-more")!;
    expect(more.textContent).toBe("More options");
    expect(more.getAttribute("aria-expanded")).toBe("false");
    expect(host.textContent).not.toContain("Model");
    click(more);
    expect(more.textContent).toBe("Fewer options");
    expect(more.getAttribute("aria-expanded")).toBe("true");
    const adv = host.querySelector(".fly-sgroup-adv")!;
    expect(adv.textContent).toBe("Model");
    expect(more.getAttribute("aria-controls")).toBe(adv.id);
  });

  it("has no footer without advanced rows", () => {
    const { host } = render(
      <SettingsGroup>
        <SettingsRow title="Microphone" />
      </SettingsGroup>,
    );
    expect(host.querySelector(".fly-sgroup-more")).toBeNull();
  });

  it("supports a controlled open state (deep links)", () => {
    const onExpandedChange = vi.fn();
    const { host, rerender } = render(
      <SettingsGroup
        expanded
        onExpandedChange={onExpandedChange}
        advanced={<SettingsRow title="Models on this computer" />}
      >
        <SettingsRow title="Quality" />
      </SettingsGroup>,
    );
    expect(host.textContent).toContain("Models on this computer");
    click(host.querySelector(".fly-sgroup-more"));
    expect(onExpandedChange).toHaveBeenCalledWith(false);
    // still open until the owner changes the prop
    expect(host.textContent).toContain("Models on this computer");
    rerender(
      <SettingsGroup expanded={false} advanced={<SettingsRow title="Models on this computer" />}>
        <SettingsRow title="Quality" />
      </SettingsGroup>,
    );
    expect(host.textContent).not.toContain("Models on this computer");
  });
});

describe("layered Escape", () => {
  it("closes only the topmost layer, then the next one", () => {
    const calls: string[] = [];
    const offA = pushEscapeLayer(() => calls.push("settings"));
    const offB = pushEscapeLayer(() => calls.push("sheet"));
    expect(escapeLayerDepth()).toBe(2);
    pressEscape();
    expect(calls).toEqual(["sheet"]);
    offB();
    pressEscape();
    expect(calls).toEqual(["sheet", "settings"]);
    offA();
    expect(escapeLayerDepth()).toBe(0);
    pressEscape();
    expect(calls).toEqual(["sheet", "settings"]);
  });

  it("marks the event handled and ignores events something else already handled", () => {
    const fn = vi.fn();
    const off = pushEscapeLayer(fn);
    const e = pressEscape();
    expect(e.defaultPrevented).toBe(true);
    // an inner control that consumed Esc first (e.g. clearing a search box)
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.addEventListener("keydown", (ev) => ev.preventDefault());
    pressEscape(input);
    expect(fn).toHaveBeenCalledTimes(1);
    input.remove();
    off();
  });

  it("ignores other keys", () => {
    const fn = vi.fn();
    const off = pushEscapeLayer(fn);
    act(() => {
      document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });
    expect(fn).not.toHaveBeenCalled();
    off();
  });

  it("useEscapeLayer keeps stack order when the callback identity changes", () => {
    const calls: string[] = [];
    function Layer({ name, tick }: { name: string; tick: number }) {
      // a fresh closure every render
      useEscapeLayer(() => calls.push(`${name}@${tick}`));
      return null;
    }
    const outer = render(<Layer name="outer" tick={0} />);
    render(<Layer name="inner" tick={0} />);
    outer.rerender(<Layer name="outer" tick={1} />);
    pressEscape();
    expect(calls).toEqual(["inner@0"]);
  });

  it("an inactive layer does not register", () => {
    const fn = vi.fn();
    function Layer({ active }: { active: boolean }) {
      useEscapeLayer(fn, active);
      return null;
    }
    const { rerender } = render(<Layer active={false} />);
    expect(escapeLayerDepth()).toBe(0);
    rerender(<Layer active />);
    expect(escapeLayerDepth()).toBe(1);
    pressEscape();
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("Modal uses the stack: Esc closes the dialog on top, not the one below", () => {
    const closeBelow = vi.fn();
    const closeTop = vi.fn();
    render(
      <Modal title="Settings" onClose={closeBelow}>
        below
      </Modal>,
    );
    render(
      <Modal title="Discard changes?" onClose={closeTop}>
        top
      </Modal>,
    );
    pressEscape();
    expect(closeTop).toHaveBeenCalledTimes(1);
    expect(closeBelow).not.toHaveBeenCalled();
  });

  it("a closed Modal holds no layer", () => {
    render(
      <Modal open={false} onClose={() => {}}>
        hidden
      </Modal>,
    );
    expect(escapeLayerDepth()).toBe(0);
  });
});

describe("Sheet", () => {
  it("renders a labelled dialog and closes on Esc and the close button", () => {
    const onClose = vi.fn();
    const { host } = render(
      <Sheet title="Answer" onClose={onClose} footer={<button>Insert</button>}>
        body
      </Sheet>,
    );
    const dialog = host.querySelector('[role="dialog"]')!;
    const title = host.querySelector(".fly-sheet-title")!;
    expect(dialog.getAttribute("aria-labelledby")).toBe(title.id);
    expect(dialog.getAttribute("aria-modal")).toBeNull();
    expect(host.querySelector(".fly-sheet--bottom")).not.toBeNull();
    expect(host.querySelector(".fly-sheet-foot")?.textContent).toBe("Insert");
    pressEscape();
    expect(onClose).toHaveBeenCalledTimes(1);
    click(host.querySelector('[aria-label="Close"]'));
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("modal sheets get a scrim that closes them; docked sheets sit on the right", () => {
    const onClose = vi.fn();
    const { host } = render(
      <Sheet title="Answer" side="right" modal onClose={onClose}>
        body
      </Sheet>,
    );
    expect(host.querySelector(".fly-sheet--right")).not.toBeNull();
    expect(host.querySelector('[role="dialog"]')!.getAttribute("aria-modal")).toBe("true");
    click(host.querySelector(".fly-sheet-scrim"));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("renders nothing and holds no Esc layer when closed", () => {
    const { host } = render(
      <Sheet open={false} onClose={() => {}}>
        body
      </Sheet>,
    );
    expect(host.innerHTML).toBe("");
    expect(escapeLayerDepth()).toBe(0);
  });
});

describe("NoticeStack", () => {
  it("orders by priority, keeping insertion order for ties", () => {
    const ordered = orderNotices([
      { id: "update", priority: 1 },
      { id: "pipeline", priority: 2 },
      { id: "audio", priority: 3 },
      { id: "pipeline-2", priority: 2 },
    ]);
    expect(ordered.map((n) => n.id)).toEqual(["audio", "pipeline", "pipeline-2", "update"]);
  });

  it("shows the top notice and a +N more toggle", () => {
    const { host } = render(
      <NoticeStack
        notices={[
          { id: "update", priority: 1, node: <Notice title="Update ready" /> },
          { id: "audio", priority: 3, node: <Notice title="Can't hear the other participants" /> },
          { id: "pipeline", priority: 2, node: <Notice tone="danger" title="Transcript failed" /> },
        ]}
      />,
    );
    const ids = () =>
      [...host.querySelectorAll("[data-notice-id]")].map((n) => n.getAttribute("data-notice-id"));
    expect(ids()).toEqual(["audio"]);
    const more = host.querySelector(".fly-notices-more")!;
    expect(more.textContent).toBe("+2 more");
    click(more);
    expect(ids()).toEqual(["audio", "pipeline", "update"]);
    expect(more.textContent).toBe("Show fewer");
    expect(more.getAttribute("aria-expanded")).toBe("true");
  });

  it("has no toggle when everything fits, and nothing when empty", () => {
    const { host, rerender } = render(
      <NoticeStack notices={[{ id: "a", priority: 1, node: <Notice title="One" /> }]} />,
    );
    expect(host.querySelector(".fly-notices-more")).toBeNull();
    rerender(<NoticeStack notices={[]} />);
    expect(host.innerHTML).toBe("");
  });

  it("Notice reads cause, numbered fix, collapsible details", () => {
    const onDismiss = vi.fn();
    const { host } = render(
      <Notice
        tone="danger"
        title="Can't hear the other participants"
        steps={["Open Sound settings", "Unmute the output"]}
        details="loopback never started"
        onDismiss={onDismiss}
      >
        Your speaker output is muted.
      </Notice>,
    );
    const root = host.firstElementChild!;
    expect(root.getAttribute("role")).toBe("alert");
    expect(host.querySelector(".fly-notice-body")?.textContent).toBe(
      "Your speaker output is muted.",
    );
    expect([...host.querySelectorAll(".fly-notice-steps li")].map((l) => l.textContent)).toEqual([
      "Open Sound settings",
      "Unmute the output",
    ]);
    expect(host.querySelector("details summary")?.textContent).toBe("Details");
    click(host.querySelector('[aria-label="Dismiss"]'));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});
