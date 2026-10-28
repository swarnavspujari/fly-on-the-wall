// @vitest-environment jsdom
import { act, createRef, useImperativeHandle, type Ref } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useDeferredDelete, type DeferredDelete } from "./deferredDelete";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

type Item = { id: string };

let root: Root | null = null;
let host: HTMLElement | null = null;
const handle = createRef<DeferredDelete<Item>>();

function Harness({
  commit,
  delay,
  ref,
}: {
  commit: (i: Item) => void;
  delay: number;
  ref: Ref<DeferredDelete<Item>>;
}) {
  const dd = useDeferredDelete(commit, delay);
  useImperativeHandle(ref, () => dd, [dd]);
  return <span data-pending={dd.pending.map((i) => i.id).join(",")} />;
}

function mount(commit: (i: Item) => void, delay = 1000) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<Harness commit={commit} delay={delay} ref={handle} />));
}

const api = {
  schedule: (i: Item) => handle.current!.schedule(i),
  undo: () => handle.current!.undo(),
  flush: () => handle.current!.flush(),
};

const pendingId = () => host!.querySelector("span")!.getAttribute("data-pending");

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.useRealTimers();
});

describe("useDeferredDelete", () => {
  it("deletes only once the toast expires", () => {
    const commit = vi.fn();
    mount(commit);
    act(() => api.schedule({ id: "a" }));
    expect(pendingId()).toBe("a");
    act(() => vi.advanceTimersByTime(999));
    expect(commit).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(1));
    expect(commit).toHaveBeenCalledWith({ id: "a" });
    expect(pendingId()).toBe("");
  });

  it("Undo cancels the delete and hands the items back", () => {
    const commit = vi.fn();
    mount(commit);
    act(() => api.schedule({ id: "a" }));
    let restored: Item[] = [];
    act(() => {
      restored = api.undo();
    });
    expect(restored).toEqual([{ id: "a" }]);
    act(() => vi.advanceTimersByTime(5000));
    expect(commit).not.toHaveBeenCalled();
    expect(pendingId()).toBe("");
  });

  it("a second delete joins the first behind one toast; neither runs early", () => {
    const commit = vi.fn();
    mount(commit);
    act(() => api.schedule({ id: "a" }));
    act(() => vi.advanceTimersByTime(600));
    act(() => api.schedule({ id: "b" }));
    expect(pendingId()).toBe("a,b");
    // the first one's original expiry passes: still waiting (timer restarted)
    act(() => vi.advanceTimersByTime(600));
    expect(commit).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(400));
    expect(commit.mock.calls).toEqual([[{ id: "a" }], [{ id: "b" }]]);
    expect(pendingId()).toBe("");
  });

  it("Undo restores every waiting note", () => {
    const commit = vi.fn();
    mount(commit);
    act(() => api.schedule({ id: "a" }));
    act(() => api.schedule({ id: "b" }));
    let restored: Item[] = [];
    act(() => {
      restored = api.undo();
    });
    expect(restored).toEqual([{ id: "a" }, { id: "b" }]);
    act(() => vi.advanceTimersByTime(5000));
    expect(commit).not.toHaveBeenCalled();
  });

  it("closing the toast deletes every waiting note now, once", () => {
    const commit = vi.fn();
    mount(commit);
    act(() => api.schedule({ id: "a" }));
    act(() => api.schedule({ id: "b" }));
    act(() => api.flush());
    expect(commit).toHaveBeenCalledTimes(2);
    act(() => vi.advanceTimersByTime(5000));
    expect(commit).toHaveBeenCalledTimes(2);
  });

  it("the app closing while the toast waits deletes NOTHING", () => {
    const commit = vi.fn();
    mount(commit);
    act(() => api.schedule({ id: "a" }));
    act(() => api.schedule({ id: "b" }));
    act(() => root!.unmount());
    root = null;
    act(() => vi.advanceTimersByTime(60_000));
    expect(commit).not.toHaveBeenCalled();
  });

  it("undo with nothing waiting is a no-op", () => {
    const commit = vi.fn();
    mount(commit);
    let restored: Item[] = [{ id: "x" }];
    act(() => {
      restored = api.undo();
    });
    expect(restored).toEqual([]);
    expect(commit).not.toHaveBeenCalled();
  });
});
