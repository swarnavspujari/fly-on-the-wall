/* Test helpers: a stand-in for the native cloud-consent dialog
   (llm_commands.rs request_cloud_consent), which no webview test can see or
   click. Like the dialog, a request waits until it's answered. Answer inside
   act() at the call site. */
import { vi } from "vitest";
import { api } from "../api";

export interface ConsentRequest {
  provider: string;
  atLaunch: boolean;
}

let waiting: (ConsentRequest & { answer: (yes: boolean) => void })[] = [];

/** Spy on api.requestCloudConsent. A yes calls `record(provider)`, as the
 *  backend records consent, and resolves true; a no resolves false. */
export function holdConsentRequests(record: (provider: string) => void) {
  waiting = [];
  return vi.spyOn(api, "requestCloudConsent").mockImplementation(
    (provider: string, atLaunch = false) =>
      new Promise<boolean>((resolve) => {
        waiting.push({
          provider,
          atLaunch,
          answer: (yes) => {
            if (yes) record(provider);
            resolve(yes);
          },
        });
      }),
  );
}

/** The oldest request still waiting for an answer, or null. */
export function consentRequest(): ConsentRequest | null {
  const q = waiting[0];
  return q ? { provider: q.provider, atLaunch: q.atLaunch } : null;
}

/** Answer the oldest waiting request. */
export function answerConsent(yes: boolean) {
  const q = waiting.shift();
  if (!q) throw new Error("no consent request is waiting");
  q.answer(yes);
}
