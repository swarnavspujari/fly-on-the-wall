/* Test helpers for Settings › Connectors: mount the section against the dev
   mock backend and find things the way a person would. Plain DOM queries;
   every interaction is wrapped in act(). */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ConfirmProvider } from "./confirm";
import { ConnectorCallStack } from "./ConnectorCallCard";
import ConnectorsSection from "./ConnectorsSection";

export interface Mounted {
  host: HTMLElement;
  root: Root;
}

export async function flush(rounds = 8) {
  for (let i = 0; i < rounds; i++) await act(async () => {});
}

export async function mountSection(): Promise<Mounted> {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  act(() =>
    root.render(
      <ConfirmProvider>
        <ConnectorsSection />
        <ConnectorCallStack />
      </ConfirmProvider>,
    ),
  );
  await flush();
  return { host, root };
}

export function unmount(m: Mounted | null) {
  if (!m) return;
  act(() => m.root.unmount());
  m.host.remove();
}

/** A button (or link-button) whose visible text is exactly `text`. */
export function buttonIn(scope: ParentNode, text: string): HTMLButtonElement | null {
  return (
    [...scope.querySelectorAll<HTMLButtonElement>("button")].find(
      (b) => b.textContent?.trim() === text,
    ) ?? null
  );
}

/** A field by its visible label. */
export function fieldIn(scope: ParentNode, label: string): HTMLInputElement {
  const l = [...scope.querySelectorAll("label[for]")].find((x) => x.textContent?.trim() === label);
  const el = l ? document.getElementById(l.getAttribute("for") ?? "") : null;
  if (!el) throw new Error(`no field labelled ${label}`);
  return el as HTMLInputElement;
}

export async function click(el: Element | null) {
  if (!el) throw new Error("element not found");
  await act(async () => (el as HTMLElement).click());
  await flush();
}

export async function type(el: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const proto =
    el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")!.set!;
  await act(async () => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
}

/** Opens a menu button and picks an item. */
export async function pickMenu(button: Element | null, item: string) {
  await click(button);
  const menu = document.querySelector('[role="menu"]');
  const choice = [...(menu?.querySelectorAll<HTMLButtonElement>('[role^="menuitem"]') ?? [])].find(
    (b) => b.textContent?.replace("✓", "").trim() === item,
  );
  await click(choice ?? null);
}

/** The section's polite live region. */
export const liveText = (host: HTMLElement) =>
  host.querySelector('[role="status"][aria-live="polite"]')?.textContent ?? "";
