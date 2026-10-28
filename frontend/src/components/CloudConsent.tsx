import { useCallback, useEffect, useRef } from "react";
import { api } from "../api";
import { cloudConsentCopy, cloudConsentLaunchIntro, providerName } from "../askModel";
import type { LlmProviderInfo } from "../types";
import { useConfirm } from "./confirm";

/* Cloud consent: the one place the app asks before a cloud provider first
   gets meeting content. Settings, Ask's Runs on and the launch check all use
   it. The backend (llm_commands.rs build_provider) refuses cloud calls
   without `consent.cloud_llm.<id>` = "true", so this is the only way that
   value gets written, and only after an explicit yes. */

export const cloudConsentKey = (id: string) => `consent.cloud_llm.${id}`;

export interface CloudConsentOptions {
  /** A first paragraph before the disclosure (the launch check's reason). */
  intro?: string;
  cancelLabel?: string;
}

/** Returns `ask(providers, id)`: true when the provider is local, already
 *  allowed, or you allow it now. Consent is saved only on that yes; if
 *  saving fails it returns false, since the backend would refuse anyway. */
export function useCloudConsent() {
  const confirm = useConfirm();
  return useCallback(
    async (
      providers: readonly LlmProviderInfo[],
      id: string,
      { intro, cancelLabel }: CloudConsentOptions = {},
    ): Promise<boolean> => {
      const info = providers.find((p) => p.id === id);
      if (!info || info.is_local) return true;
      const key = cloudConsentKey(id);
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
        return true;
      } catch {
        return false;
      }
    },
    [confirm],
  );
}

/** At launch, if the saved provider is a cloud one you haven't allowed, ask
 *  once (in-app, with the disclosure). Launch is the one moment before every
 *  LLM use, including the cleanup and action items that run by themselves
 *  after a transcription, where there's no click to ask on. "Not now" saves
 *  nothing: the backend keeps refusing, and Settings › Ask and Enhance (Test)
 *  or Ask's "Review and allow" can still ask later. */
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
