import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, LLM_SETTINGS_CHANGED_EVENT } from "../api";
import { cloudConsentCopy, cloudConsentLaunchIntro, providerName } from "../askModel";
import type { LlmProviderInfo } from "../types";
import { useConfirm } from "./confirm";

/* Cloud consent: the one place the app asks before a cloud provider first
   gets meeting content. Settings, Ask's Runs on, the launch check and the
   skipped-cleanup notice all use it. The backend (llm_commands.rs
   build_provider) refuses cloud calls without `consent.cloud_llm.<id>` =
   "true", and this is the only code that writes that value, only after an
   explicit yes. */

export const cloudConsentKey = (id: string) => `consent.cloud_llm.${id}`;

export interface CloudConsentOptions {
  /** A first paragraph before the disclosure (the launch check's reason). */
  intro?: string;
  cancelLabel?: string;
}

/* One question per provider at a time: a second ask for the same provider
   (the launch check and Ask's "Review and allow", a double click) waits for
   the first answer instead of queueing a second dialog. */
const inFlight = new Map<string, Promise<boolean>>();

/** Returns `ask(providers, id)`: true when the provider is local, already
 *  allowed, or you allow it now. Consent is saved only on that yes; if
 *  saving fails it returns false, since the backend would refuse anyway. An
 *  id that isn't in `providers` is refused without asking. */
export function useCloudConsent() {
  const confirm = useConfirm();
  return useCallback(
    (
      providers: readonly LlmProviderInfo[],
      id: string,
      { intro, cancelLabel }: CloudConsentOptions = {},
    ): Promise<boolean> => {
      const info = providers.find((p) => p.id === id);
      if (!info) return Promise.resolve(false);
      if (info.is_local) return Promise.resolve(true);
      const pending = inFlight.get(id);
      if (pending) return pending;
      const key = cloudConsentKey(id);
      const asking = (async () => {
        try {
          if ((await api.getAppSetting(key)) === "true") return true;
        } catch {
          /* fall through to asking */
        }
        const copy = cloudConsentCopy(providerName(id));
        const ok = await confirm({
          title: copy.title,
          message: intro ? `${intro}\n\n${copy.message}` : copy.message,
          confirmLabel: copy.confirmLabel,
          cancelLabel,
        });
        if (!ok) return false;
        try {
          await api.setAppSetting(key, "true");
          window.dispatchEvent(new Event(LLM_SETTINGS_CHANGED_EVENT));
          return true;
        } catch {
          return false;
        }
      })().finally(() => inFlight.delete(id));
      inFlight.set(id, asking);
      return asking;
    },
    [confirm],
  );
}

/** At launch, if the saved provider is a cloud one you haven't allowed, ask
 *  once (in-app, with the disclosure). Launch is the one moment before every
 *  LLM use, including the cleanup and action items that run by themselves
 *  after a transcription, where there's no click to ask on. "Not now" saves
 *  nothing: the backend keeps refusing, and Settings › Ask and Enhance (Test),
 *  Ask's "Review and allow" and the skipped-cleanup notice can ask later. */
export function CloudConsentOnLaunch() {
  const ask = useCloudConsent();
  const asked = useRef(false);
  useEffect(() => {
    if (asked.current) return;
    asked.current = true;
    void (async () => {
      try {
        const s = await api.getLlmSettings();
        await ask(s.providers, s.provider, {
          intro: cloudConsentLaunchIntro(providerName(s.provider)),
          cancelLabel: "Not now",
        });
      } catch {
        /* settings unreadable: nothing is sent without consent anyway */
      }
    })();
  }, [ask]);
  return null;
}

export interface ConsentSkips {
  /** Meetings waiting (0 once dismissed, or while the re-run starts). */
  count: number;
  /** Who needs the OK, or null when the saved provider doesn't (allowed
   *  meanwhile, or local now): then running them is all that's left. */
  vendor: string | null;
  busy: boolean;
  /** Ask for consent (if needed), then run the skipped cleanup and action
   *  items in the background. */
  run: () => Promise<void>;
  dismiss: () => void;
}

/** Background cleanup and action items the backend skipped for missing
 *  consent this session (`consent_skipped_meetings`, then each
 *  `llm:consent-skips` update). Dismissing hides the notice for the session. */
export function useConsentSkips(): ConsentSkips {
  const ask = useCloudConsent();
  const [skipped, setSkipped] = useState<string[]>([]);
  const [vendor, setVendor] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | null = null;
    api
      .consentSkippedMeetings()
      .then((ids) => {
        if (live) setSkipped(ids);
      })
      .catch(() => {});
    listen<string[]>("llm:consent-skips", (e) => setSkipped(e.payload))
      .then((un) => {
        if (live) unlisten = un;
        else un();
      })
      .catch(() => {});
    return () => {
      live = false;
      unlisten?.();
    };
  }, []);

  // Who to name: the saved provider, unless it's local or already allowed.
  // Read again whenever the provider or a consent changes (Settings, Ask),
  // so the copy never names a provider that no longer needs the OK.
  const waiting = skipped.length > 0;
  const [settingsVersion, setSettingsVersion] = useState(0);
  useEffect(() => {
    const changed = () => setSettingsVersion((v) => v + 1);
    window.addEventListener(LLM_SETTINGS_CHANGED_EVENT, changed);
    return () => window.removeEventListener(LLM_SETTINGS_CHANGED_EVENT, changed);
  }, []);
  useEffect(() => {
    if (!waiting) return;
    let live = true;
    void (async () => {
      try {
        const s = await api.getLlmSettings();
        const info = s.providers.find((p) => p.id === s.provider);
        const allowed =
          !!info?.is_local || (await api.getAppSetting(cloudConsentKey(s.provider))) === "true";
        if (live) setVendor(allowed ? null : providerName(s.provider));
      } catch {
        /* keep the last answer */
      }
    })();
    return () => {
      live = false;
    };
  }, [waiting, settingsVersion]);

  const run = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const s = await api.getLlmSettings();
      if (!(await ask(s.providers, s.provider))) return;
      await api.retryConsentSkipped();
      setSkipped([]);
    } catch {
      /* the backend still refuses without consent; the notice stays */
    } finally {
      setBusy(false);
    }
  };

  return {
    count: dismissed ? 0 : skipped.length,
    vendor,
    busy,
    run,
    dismiss: () => setDismissed(true),
  };
}
