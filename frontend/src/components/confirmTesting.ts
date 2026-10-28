/* Test helpers for the in-app confirm dialog (confirm.tsx). Plain DOM
   queries: wrap clicks in act() at the call site. */

/** The open confirm dialog, or null. */
export const confirmDialog = () =>
  document.querySelector<HTMLElement>('[data-testid="confirm-dialog"]');

/** The open dialog's question. */
export const confirmTitle = () =>
  confirmDialog()?.querySelector(".fly-modal-title")?.textContent ?? null;

/** The yes button (`true`) or Cancel (`false`) of the open dialog. Cancel is
 *  the one that starts with focus. */
export function confirmChoice(answer: boolean): HTMLButtonElement {
  const dialog = confirmDialog();
  if (!dialog) throw new Error("no confirm dialog is open");
  const cancel = dialog.querySelector<HTMLButtonElement>(".fly-modal-foot [data-autofocus]");
  const yes = [...dialog.querySelectorAll<HTMLButtonElement>(".fly-modal-foot button")].find(
    (b) => b !== cancel,
  );
  if (!cancel || !yes) throw new Error("confirm dialog is missing a button");
  return answer ? yes : cancel;
}
