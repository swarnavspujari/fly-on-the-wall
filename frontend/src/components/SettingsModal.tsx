import type { CSSProperties, ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { ArrowLeft, Check, Copy, Plus } from "lucide-react";
import { api } from "../api";
import { RESTART_REQUIRED_EVENT } from "../privacy";
import { WHISPER_ENGINE_ID } from "../types";
import type {
  AppInfo,
  AsrSettings,
  AudioDevice,
  CalendarStatus,
  CalendarToggle,
  LlmSettings,
  ModelProgress,
  OllamaStatus,
  Template,
} from "../types";
import {
  Badge,
  Button,
  Card,
  Input,
  Modal,
  ProgressBar,
  Segmented,
  Select,
  SettingsGroup,
  SettingsRow,
  Switch,
  useEscapeLayer,
  useDialogFocus,
  EscapeLayerScope,
} from "./ui";
import { useTheme } from "../theme";
import { briefError } from "../pipelineNotice";
import { useCloudConsent } from "./CloudConsent";
import { useConfirm } from "./confirm";
import PrivacySection from "./PrivacySection";
import RemindNoticeRow from "./RemindNoticeRow";
import type { Updater } from "../updater";
import {
  activeSectionAt,
  asrDraftFrom,
  asrUpdateFrom,
  claudeCodeCommand,
  dataTiles,
  effectiveModel,
  isAsrDirty,
  madeOnHelper,
  matchSections,
  mcpCommand,
  providerLabel,
  providerVendor,
  qualityHelper,
  serverChangeNeedsKey,
  serverChanged,
  type AsrDraft,
  type MadeOn,
  type Quality,
  type SectionId,
  type SettingsFocus,
} from "../settingsModel";

interface Props {
  modelProgress: ModelProgress | null;
  updater: Updater;
  recordingActive: boolean;
  appVersion: string | null;
  /** Deep-link from the transcribe error: "engine" opens Transcripts › More
   *  options scrolled to "Models on this computer"; "groq" scrolls to the
   *  "Made on" row, where Groq is one click away. */
  initialFocus?: SettingsFocus | null;
  onClose: () => void;
}

/* Textarea chrome matched to the token field recipe (see USAGE.md). */
const TEXTAREA: CSSProperties = {
  width: "100%",
  fontFamily: "var(--font-sans)",
  fontSize: "13px",
  lineHeight: 1.5,
  color: "var(--text)",
  background: "var(--surface-2)",
  border: "1px dashed var(--line)",
  borderRadius: "var(--radius-md)",
  padding: "8px 12px",
  outline: "none",
  resize: "vertical",
};

/** Claude models offered for the Anthropic provider; the first is the default. */
const ANTHROPIC_MODELS = [
  { value: "claude-sonnet-5-5", label: "Claude Sonnet 5.5 (recommended)" },
  { value: "claude-haiku-5-5", label: "Claude Haiku 5.5 (faster, lower cost)" },
];

const QUALITY_OPTIONS: { value: Quality; label: string }[] = [
  { value: "light", label: "Fast" },
  { value: "balanced", label: "Balanced" },
  { value: "best", label: "Best" },
];

const MADE_ON_OPTIONS: { value: MadeOn; label: string }[] = [
  { value: "local", label: "This computer" },
  { value: "groq", label: "Groq" },
  { value: "hybrid", label: "Groq, then this computer" },
];

const TIER_NAMES: Record<string, string> = {
  light: "Fast",
  balanced: "Balanced",
  best: "Best",
  cloud: "Groq",
};

const CALENDAR_NAMES: Record<string, string> = {
  google: "Google Calendar",
  msgraph: "Outlook",
};

const MONO_KEY: CSSProperties = { fontFamily: "var(--font-mono)", fontSize: "12px" };
const SMALL: CSSProperties = { margin: 0, fontSize: 12, lineHeight: 1.5 };

/** After a save that moved a cloud provider to another server, when the
 *  consent dialog for that server was declined. */
const NEW_SERVER_NOT_ALLOWED =
  "Saved. Nothing is sent to the new server until you allow it (Test asks again).";
const MONO_SMALL: CSSProperties = { fontFamily: "var(--font-mono)", fontSize: 11 };
const LIST_ROW =
  "flex items-center justify-between gap-3 border-t border-line-2 py-2 first:border-t-0";

function gb(bytes: number): string {
  return bytes >= 1_000_000_000
    ? `${(bytes / 1_000_000_000).toFixed(1)} GB`
    : `${Math.round(bytes / 1_000_000)} MB`;
}

const prefersReducedMotion = () =>
  typeof window !== "undefined" &&
  !!window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;

/** A section heading plus its group(s); `data-section` drives scroll-spy.
 *  Search hides sections rather than unmounting them, so a half-filled
 *  form (retention confirm, delete-all) survives typing in the search box. */
function Section({
  id,
  title,
  hidden,
  children,
}: {
  id: SectionId;
  title: string;
  hidden: boolean;
  children: ReactNode;
}) {
  return (
    <section
      data-section={id}
      hidden={hidden}
      aria-labelledby={`settings-h-${id}`}
      className="fly-settings-section"
    >
      <h2 id={`settings-h-${id}`} className="fly-settings-h">
        {title}
      </h2>
      {children}
    </section>
  );
}

/** Ink card holding a copyable snippet (MCP config, Claude Code command). */
function SnippetCard({ label, text }: { label: string; text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div
      style={{
        background: "var(--brand-ink)",
        color: "var(--brand-cream)",
        borderRadius: "var(--radius-lg)",
        padding: "12px 14px",
        display: "flex",
        flexDirection: "column",
        gap: 8,
      }}
    >
      <div className="flex items-center justify-between gap-2">
        <span style={{ fontSize: 12, fontWeight: 600 }}>{label}</span>
        <Button
          variant="soft"
          size="xs"
          startIcon={
            copied ? <Check size={13} strokeWidth={1.75} /> : <Copy size={13} strokeWidth={1.75} />
          }
          onClick={() => {
            void navigator.clipboard.writeText(text).then(() => {
              setCopied(true);
              window.setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          {copied ? "Copied" : "Copy"}
        </Button>
      </div>
      <pre
        style={{
          margin: 0,
          overflowX: "auto",
          whiteSpace: "pre",
          fontFamily: "var(--font-mono)",
          fontSize: 11.5,
          lineHeight: 1.6,
          color: "var(--brand-cream)",
        }}
      >
        {text}
      </pre>
    </div>
  );
}

export default function SettingsModal({
  modelProgress,
  updater,
  recordingActive,
  appVersion,
  initialFocus = null,
  onClose,
}: Props) {
  /* ---------------------------------------------------------- transcripts */
  // `settings` is what's saved; `draft` is the Transcripts group's unsaved
  // edit, applied all-or-nothing by set_asr_settings via the save bar.
  const [settings, setSettings] = useState<AsrSettings | null>(null);
  const [draft, setDraft] = useState<AsrDraft | null>(null);
  const [groqKey, setGroqKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  // "Transcribe when I stop" applies at once (it lives in Recording).
  const [autoTranscribe, setAutoTranscribe] = useState(true);
  const [autoBusy, setAutoBusy] = useState(false);
  const [autoMsg, setAutoMsg] = useState<string | null>(null);
  const [gpuBusy, setGpuBusy] = useState(false);
  const [gpuMsg, setGpuMsg] = useState<string | null>(null);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [downloadErrors, setDownloadErrors] = useState<Record<string, string>>({});
  const [backfillBusy, setBackfillBusy] = useState(false);
  const [backfillMsg, setBackfillMsg] = useState<string | null>(null);
  // The backend's own words when every meeting failed (muted, under the message)
  const [backfillDetail, setBackfillDetail] = useState<string | null>(null);
  // null = count not loaded yet (generic label until it arrives)
  const [backfillPending, setBackfillPending] = useState<number | null>(null);
  const [confirmClose, setConfirmClose] = useState(false);

  // Appearance control (System / Light / Dark) — wired to the shared theme hook.
  const { theme, setTheme } = useTheme();

  const confirm = useConfirm();
  const askCloudConsent = useCloudConsent();

  // LLM provider state
  const [llm, setLlm] = useState<LlmSettings | null>(null);
  const [llmProvider, setLlmProvider] = useState("ollama");
  const [llmModel, setLlmModel] = useState("");
  const [llmBaseUrl, setLlmBaseUrl] = useState("");
  const [llmKey, setLlmKey] = useState("");
  const [llmTest, setLlmTest] = useState<string | null>(null);
  const [llmBusy, setLlmBusy] = useState(false);
  const [llmLoadError, setLlmLoadError] = useState<string | null>(null);

  // managed Ollama (local provider) state
  const [ollama, setOllama] = useState<OllamaStatus | null>(null);
  const [ollamaBusy, setOllamaBusy] = useState<"install" | "pull" | null>(null);
  const [ollamaMsg, setOllamaMsg] = useState<string | null>(null);
  // Guided model swap: `swapOffer` = the user saved an Ollama model that
  // isn't installed (offer pull-with-progress instead of failing at first
  // use); `deleteOffer` = a swap completed and the old model still sits on
  // disk (offer — never force — deleting it).
  const [swapOffer, setSwapOffer] = useState<{ to: string; from: string | null } | null>(null);
  const [deleteOffer, setDeleteOffer] = useState<{ name: string; size: number } | null>(null);

  // templates state
  const [templates, setTemplates] = useState<Template[]>([]);
  const [editingTpl, setEditingTpl] = useState<Template | null>(null);
  const [templatesOpen, setTemplatesOpen] = useState(false);

  // MCP state
  const [mcpJson, setMcpJson] = useState("");
  const [mcpSetupOpen, setMcpSetupOpen] = useState(false);

  // microphone
  const [mics, setMics] = useState<AudioDevice[]>([]);
  const [micId, setMicId] = useState("");

  // calendar state
  const [cal, setCal] = useState<CalendarStatus | null>(null);
  const [googleId, setGoogleId] = useState("");
  const [googleSecret, setGoogleSecret] = useState("");
  const [msId, setMsId] = useState("");
  const [calBusy, setCalBusy] = useState<string | null>(null);
  const [calMsg, setCalMsg] = useState<string | null>(null);
  // Per-calendar on/off toggles (which calendars feed "Up next").
  const [calList, setCalList] = useState<CalendarToggle[]>([]);

  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);

  /* ---------------------------------------------------------- navigation */
  const [query, setQuery] = useState("");
  const [active, setActive] = useState<SectionId>("recording");
  // Per-group "More options". A deep link to the engine row opens Transcripts.
  const [advOpen, setAdvOpen] = useState<Partial<Record<SectionId, boolean>>>(() => ({
    transcripts: initialFocus === "engine",
  }));
  const [flash, setFlash] = useState<SettingsFocus | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const spyLock = useRef(0);
  const didFocusRef = useRef(false);

  const setAdv = (id: SectionId, open: boolean) => setAdvOpen((a) => ({ ...a, [id]: open }));
  const patchDraft = (p: Partial<AsrDraft>) => {
    setDraft((d) => (d ? { ...d, ...p } : d));
    // The key field hides when Made on goes back to this computer; a typed
    // key must not stay pending where you can't see it.
    if (p.madeOn === "local") setGroqKey("");
  };
  // Instant-apply ASR writes send the saved snapshot; never let one race
  // the save bar (set_asr_settings applies in arrival order).
  const asrBusy = saving || autoBusy || gpuBusy;
  const asrDirty = !!draft && isAsrDirty(draft, settings, groqKey);

  /* ---------------------------------------------------------- loading */

  const loadCal = () =>
    api.getCalendarSettings().then((c) => {
      setCal(c);
      setGoogleId(c.google_client_id);
      setMsId(c.ms_client_id);
    });

  // Best-effort: lists calendars for the connected providers (empty if none).
  const loadCalList = () =>
    api
      .listCalendars()
      .then(setCalList)
      .catch(() => setCalList([]));

  /** Saved transcript settings + a fresh draft (initial load, after save). */
  const load = () =>
    api.getAsrSettings().then((s) => {
      setSettings(s);
      setDraft(asrDraftFrom(s));
      setAutoTranscribe(s.auto_transcribe);
    });

  /** Saved settings only (installs, GPU re-test): keeps unsaved edits. */
  const refreshSaved = () => api.getAsrSettings().then(setSettings);

  const loadLlm = () =>
    api.getLlmSettings().then(
      (s) => {
        setLlm(s);
        setLlmLoadError(null);
        setLlmProvider(s.provider);
        const info = s.providers.find((p) => p.id === s.provider);
        setLlmModel(info?.model ?? "");
        setLlmBaseUrl(info?.base_url ?? "");
      },
      (e) => {
        // Keep the rest of the section usable (templates) and say why.
        setLlmLoadError(`Couldn't load the AI settings: ${String(e)}`);
        throw e;
      },
    );

  const loadOllama = () => api.ollamaStatus().then(setOllama);

  useEffect(() => {
    load().catch(console.error);
    loadLlm().catch(console.error);
    loadOllama().catch(console.error);
    api.listTemplates().then(setTemplates).catch(console.error);
    loadCal().catch(console.error);
    loadCalList().catch(console.error);
    api.mcpConfig().then(setMcpJson).catch(console.error);
    api.listMicDevices().then(setMics).catch(console.error);
    api.appInfo().then(setAppInfo).catch(console.error);
    api
      .getAppSetting("capture.mic_device_id")
      .then((v) => setMicId(v ?? ""))
      .catch(console.error);
    // Count of un-extracted meetings behind the search-index button.
    void api
      .pendingBackfillCount()
      .then(setBackfillPending)
      .catch(() => {});
  }, []);

  /* ---------------------------------------------------------- overlay */

  /** Back, Done and Esc: unsaved transcript changes ask first. */
  const requestClose = () => {
    if (asrDirty) setConfirmClose(true);
    else onClose();
  };
  const escDepth = useEscapeLayer(requestClose);

  // Focus moves in on open (the search box, unless deep-linked), Tab stays
  // inside, and focus goes back to whatever had it when Settings closes, so
  // you land where you were.
  const overlayRef = useRef<HTMLDivElement>(null);
  useDialogFocus(overlayRef, true, { trap: true });

  // After "Delete all my data" the app-level RestartRequired screen takes
  // over; Settings closes so its focus trap can't hold the keyboard under it
  // (nothing here is usable until the restart anyway).
  const closeRef = useRef(onClose);
  useEffect(() => {
    closeRef.current = onClose;
  });
  useEffect(() => {
    const close = () => closeRef.current();
    window.addEventListener(RESTART_REQUIRED_EVENT, close);
    return () => window.removeEventListener(RESTART_REQUIRED_EVENT, close);
  }, []);

  // Deep link: once settings have loaded (the target row is rendered),
  // bring it into view a single time — not on every later refresh.
  useEffect(() => {
    const keyRow = initialFocus === "llm-key";
    if (!initialFocus || !(keyRow ? llm : settings) || didFocusRef.current) return;
    didFocusRef.current = true;
    const el = scrollRef.current?.querySelector<HTMLElement>(
      keyRow
        ? "#settings-row-llm"
        : initialFocus === "groq"
          ? "#settings-row-groq"
          : "#settings-row-engine",
    );
    spyLock.current = Date.now();
    setActive(keyRow ? "ai" : "transcripts");
    setFlash(initialFocus);
    el?.scrollIntoView({ block: "center" });
    if (keyRow) el?.querySelector<HTMLInputElement>('input[type="password"]')?.focus();
  }, [initialFocus, settings, llm]);

  const visible = matchSections(query);
  const visibleIds = new Set(visible.map((s) => s.id));
  const show = (id: SectionId) => visibleIds.has(id);

  const goSection = (id: SectionId) => {
    const sc = scrollRef.current;
    const el = sc?.querySelector<HTMLElement>(`[data-section="${id}"]`);
    spyLock.current = Date.now();
    setActive(id);
    if (sc && el) {
      sc.scrollTo?.({
        top: Math.max(0, el.offsetTop - 20),
        behavior: prefersReducedMotion() ? "auto" : "smooth",
      });
    }
  };

  const onScroll = () => {
    if (Date.now() - spyLock.current < 700) return;
    const sc = scrollRef.current;
    if (!sc) return;
    const tops = [...sc.querySelectorAll<HTMLElement>("[data-section]:not([hidden])")].map(
      (el) => ({
        id: el.dataset.section as SectionId,
        top: el.offsetTop,
      }),
    );
    const atBottom = sc.scrollTop + sc.clientHeight >= sc.scrollHeight - 2;
    const id = activeSectionAt(tops, sc.scrollTop, 80, atBottom);
    if (id && id !== active) setActive(id);
  };

  const onQuery = (q: string) => {
    setQuery(q);
    const matches = matchSections(q);
    if (matches[0]) setActive(matches[0].id);
    // A match can live in "More options" (e.g. "logs" → Diagnostics), so
    // searching opens the matched groups; you can still collapse them.
    if (q.trim()) {
      setAdvOpen((a) => ({ ...a, ...Object.fromEntries(matches.map((s) => [s.id, true])) }));
    }
    scrollRef.current?.scrollTo?.({ top: 0 });
  };

  /* ---------------------------------------------------------- recording */

  const setAutoNow = async (next: boolean) => {
    if (!settings) return;
    setAutoTranscribe(next);
    setAutoBusy(true);
    setAutoMsg(null);
    try {
      // Saved values for everything else: the Transcripts draft stays unsaved.
      await api.setAsrSettings(asrUpdateFrom(asrDraftFrom(settings), settings, next, ""));
      setSettings((s) => (s ? { ...s, auto_transcribe: next } : s));
    } catch (e) {
      setAutoTranscribe(!next);
      setAutoMsg(String(e));
    } finally {
      setAutoBusy(false);
    }
  };

  /* ---------------------------------------------------------- transcripts */

  const save = async (): Promise<boolean> => {
    if (!settings || !draft) return false;
    setSaving(true);
    setSaveError(null);
    try {
      await api.setAsrSettings(asrUpdateFrom(draft, settings, autoTranscribe, groqKey));
      setGroqKey("");
      await load();
      return true;
    } catch (e) {
      setSaveError(String(e));
      return false;
    } finally {
      setSaving(false);
    }
  };

  const discard = () => {
    if (settings) setDraft(asrDraftFrom(settings));
    setGroqKey("");
    setSaveError(null);
  };

  /** Turning GPU off then on clears the stored speed-test verdict, so the
   *  next transcription measures again (backend: set_asr_settings). */
  const testGpuAgain = async () => {
    if (!settings) return;
    setGpuBusy(true);
    setGpuMsg(null);
    try {
      const base = asrUpdateFrom(asrDraftFrom(settings), settings, autoTranscribe, "");
      await api.setAsrSettings({ ...base, use_gpu: false });
      await api.setAsrSettings({ ...base, use_gpu: true });
    } catch (e) {
      setGpuMsg(String(e));
    } finally {
      setGpuBusy(false);
      refreshSaved().catch(console.error);
    }
  };

  const download = async (id: string) => {
    setDownloading(id);
    setDownloadErrors((prev) => {
      const next = { ...prev };
      delete next[id];
      return next;
    });
    try {
      await api.downloadModel(id);
      await refreshSaved();
    } catch (e) {
      console.error(e);
      setDownloadErrors((prev) => ({ ...prev, [id]: String(e) }));
    } finally {
      setDownloading(null);
    }
  };

  const runBackfill = async () => {
    // Backfill sends transcripts to the saved provider: same consent as Test
    // and Save (the backend refuses without it anyway).
    if (!(await confirmCloudProvider(llm?.provider ?? llmProvider))) {
      setBackfillMsg("Nothing was extracted. Cloud processing wasn't confirmed.");
      return;
    }
    setBackfillBusy(true);
    setBackfillMsg(null);
    setBackfillDetail(null);
    try {
      const r = await api.backfillMeetingItems();
      const allFailed = r.processed > 0 && r.failed === r.processed && !!r.first_error;
      setBackfillMsg(
        r.processed === 0
          ? "All meetings are already extracted."
          : allFailed
            ? `Couldn't extract from ${r.processed === 1 ? "the meeting" : `any of the ${r.processed} meetings`}. ` +
              "Check Ask and Enhance in Settings, then try again."
            : `${r.processed} meeting(s) processed, ${r.extracted} item(s) extracted` +
              (r.failed ? `, ${r.failed} failed` : "") +
              ".",
      );
      // every meeting failed the same way: show why, briefly
      if (allFailed) setBackfillDetail(briefError(r.first_error!, 200));
    } catch (e) {
      setBackfillMsg(String(e));
    } finally {
      setBackfillBusy(false);
      void api
        .pendingBackfillCount()
        .then(setBackfillPending)
        .catch(() => {});
    }
  };

  /* ---------------------------------------------------------- ask and enhance */

  const installOllama = async () => {
    setOllamaBusy("install");
    setOllamaMsg(null);
    try {
      await api.downloadModel("ollama-bin");
      setOllamaMsg("Installed.");
    } catch (e) {
      setOllamaMsg(String(e));
    } finally {
      setOllamaBusy(null);
      loadOllama().catch(console.error);
    }
  };

  /** Tag-insensitive match, mirroring the backend: a bare name ("llama3.1")
   *  matches any tag of its family ("llama3.1:latest"); two explicit tags
   *  must be identical. */
  const sameOllamaModel = (a: string, b: string) => {
    if (a === b) return true;
    const ab = a.split(":")[0];
    const bb = b.split(":")[0];
    return (a === ab || b === bb) && ab === bb;
  };

  const installedOllamaModel = (name: string) =>
    ollama?.models.find((m) => sameOllamaModel(m.name, name)) ?? null;

  const pullOllamaModel = async (model: string) => {
    setOllamaBusy("pull");
    setOllamaMsg(null);
    try {
      await api.ollamaPull(model);
      setOllamaMsg(`${model} is ready.`);
      // A guided swap just finished downloading its target: close the offer
      // and, if the previous model is still on disk, offer to reclaim it.
      if (swapOffer && sameOllamaModel(model, swapOffer.to)) {
        const from = swapOffer.from;
        setSwapOffer(null);
        const old = from ? installedOllamaModel(from) : null;
        if (old && !sameOllamaModel(old.name, model)) setDeleteOffer(old);
      }
    } catch (e) {
      setOllamaMsg(String(e));
    } finally {
      setOllamaBusy(null);
      loadOllama().catch(console.error);
    }
  };

  const deleteOllamaModel = async (model: string) => {
    const ok = await confirm({
      title: `Delete ${model}?`,
      message: "It's removed from this computer. You can download it again later.",
      confirmLabel: "Delete model",
      tone: "danger",
    });
    if (!ok) return;
    setOllamaMsg(null);
    try {
      await api.ollamaDelete(model);
      setOllamaMsg(`${model} deleted.`);
      setDeleteOffer((d) => (d && sameOllamaModel(d.name, model) ? null : d));
    } catch (e) {
      setOllamaMsg(String(e));
    } finally {
      loadOllama().catch(console.error);
    }
  };

  /** One-time disclosure before a cloud provider first receives meeting
   *  content: who processes it and what is sent. Accepted per provider. */
  const confirmCloudProvider = (provider: string): Promise<boolean> =>
    askCloudConsent(llm?.providers ?? [], provider);

  const pickProvider = async (id: string) => {
    // Disclose cloud processing before the provider is persisted (picking
    // saves immediately). The backend enforces the same consent.
    if (!(await confirmCloudProvider(id))) {
      setLlmTest("Kept your current provider. Nothing was sent anywhere.");
      return;
    }
    const prev = { provider: llmProvider, model: llmModel, baseUrl: llmBaseUrl };
    setLlmProvider(id);
    const info = llm?.providers.find((p) => p.id === id);
    setLlmModel(info?.model ?? "");
    setLlmBaseUrl(info?.base_url ?? "");
    setLlmKey("");
    setLlmTest(null);
    // Persist the pick immediately (optimistic, like toggleCalendar). Save
    // still owns model / base-url / key edits.
    api
      .setLlmSettings({
        provider: id,
        model: info?.model ?? null,
        base_url: info?.base_url ?? null,
        api_key: null,
      })
      .then(() => setLlm((s) => (s ? { ...s, provider: id } : s)))
      .catch((e) => {
        // Revert the fields too, so a later Save/Test can't write this
        // provider's model or URL into the previous one.
        setLlmProvider(prev.provider);
        setLlmModel(prev.model);
        setLlmBaseUrl(prev.baseUrl);
        setLlmTest(String(e));
      });
  };

  /** Whether the Base URL being saved points the provider at another server
   *  than the saved one (the backend then asks for consent again). */
  const movedServer = () => {
    const saved = llm?.providers.find((p) => p.id === llmProvider);
    return !!saved && !saved.is_local && serverChanged(llmProvider, saved.base_url, llmBaseUrl);
  };

  const saveLlm = async () => {
    if (!(await confirmCloudProvider(llmProvider))) {
      setLlmTest("Not saved. Cloud processing wasn't confirmed.");
      return;
    }
    setLlmBusy(true);
    setLlmTest(null);
    // The Ollama model that was saved BEFORE this save — the "old" side of a
    // guided swap (llm state is reloaded below).
    const ollamaInfo = llm?.providers.find((p) => p.id === "ollama");
    const prevModel = (ollamaInfo?.model ?? ollamaInfo?.default_model ?? "").trim() || null;
    const moved = movedServer();
    try {
      await api.setLlmSettings({
        provider: llmProvider,
        model: llmModel || null,
        base_url: llmBaseUrl || null,
        api_key: llmKey ? llmKey : null,
      });
      setLlmKey("");
      await loadLlm();
      setLlmTest("Saved.");
      // A new server clears the provider's consent: ask now, naming it.
      if (moved && !(await confirmCloudProvider(llmProvider))) {
        setLlmTest(NEW_SERVER_NOT_ALLOWED);
      }
      // Guided swap: a just-saved Ollama model that isn't installed would
      // fail at first use — offer the pull (with progress) right here.
      if (llmProvider === "ollama") {
        const target = (llmModel || ollamaInfo?.default_model || "llama3.1").trim();
        const from = prevModel && !sameOllamaModel(prevModel, target) ? prevModel : null;
        if (!installedOllamaModel(target)) {
          setSwapOffer({ to: target, from });
        } else if (from) {
          // Swap to an already-installed model: offer reclaiming the old one.
          const old = installedOllamaModel(from);
          if (old) setDeleteOffer(old);
        }
      }
    } catch (e) {
      setLlmTest(String(e));
    } finally {
      setLlmBusy(false);
    }
  };

  /** The Claude model select applies at once (the key and endpoint fields
   *  still need Save). The saved base URL is sent back unchanged. It holds
   *  llmBusy like Save/Test, and the select is disabled while they run, so
   *  this write can't land after an Endpoint save and undo it. */
  const saveClaudeModel = async (model: string) => {
    if (llmBusy) return;
    const prev = llmModel;
    setLlmModel(model);
    if (!(await confirmCloudProvider("anthropic"))) {
      setLlmModel(prev);
      return;
    }
    const info = llm?.providers.find((p) => p.id === "anthropic");
    setLlmBusy(true);
    try {
      await api.setLlmSettings({
        provider: "anthropic",
        model,
        base_url: info?.base_url ?? null,
        api_key: null,
      });
      setLlm((s) =>
        s
          ? {
              ...s,
              providers: s.providers.map((p) => (p.id === "anthropic" ? { ...p, model } : p)),
            }
          : s,
      );
    } catch (e) {
      setLlmModel(prev);
      setLlmTest(String(e));
    } finally {
      setLlmBusy(false);
    }
  };

  const testLlm = async () => {
    if (!(await confirmCloudProvider(llmProvider))) return;
    setLlmBusy(true);
    setLlmTest("Testing…");
    const moved = movedServer();
    try {
      await api.setLlmSettings({
        provider: llmProvider,
        model: llmModel || null,
        base_url: llmBaseUrl || null,
        api_key: llmKey ? llmKey : null,
      });
      setLlmKey("");
      if (moved && !(await confirmCloudProvider(llmProvider))) {
        setLlmTest(NEW_SERVER_NOT_ALLOWED);
        await loadLlm();
        return;
      }
      setLlmTest(await api.testLlmConnection());
      await loadLlm();
    } catch (e) {
      setLlmTest(String(e));
    } finally {
      setLlmBusy(false);
      // Testing the ollama provider may have started the managed server.
      loadOllama().catch(console.error);
    }
  };

  const saveTpl = async () => {
    if (!editingTpl) return;
    await api.saveTemplate(editingTpl);
    setEditingTpl(null);
    setTemplates(await api.listTemplates());
  };

  const removeTpl = async (id: string) => {
    await api.deleteTemplate(id);
    setTemplates(await api.listTemplates());
  };

  /* ---------------------------------------------------------- calendars */

  const toggleCalendar = async (c: CalendarToggle, enabled: boolean) => {
    setCalList((list) =>
      list.map((x) => (x.provider === c.provider && x.id === c.id ? { ...x, enabled } : x)),
    );
    try {
      await api.setCalendarEnabled(c.provider, c.id, enabled);
    } catch (e) {
      // revert the optimistic flip on failure
      setCalList((list) =>
        list.map((x) =>
          x.provider === c.provider && x.id === c.id ? { ...x, enabled: !enabled } : x,
        ),
      );
      setCalMsg(String(e));
    }
  };

  const saveCal = async () => {
    try {
      await api.setCalendarSettings({
        google_client_id: googleId,
        google_client_secret: googleSecret ? googleSecret : null,
        ms_client_id: msId,
      });
      setGoogleSecret("");
      await loadCal();
      setCalMsg("Saved.");
    } catch (e) {
      setCalMsg(String(e));
    }
  };

  const connectCal = async (provider: string) => {
    setCalBusy(provider);
    setCalMsg("A browser window opened. Finish signing in there.");
    try {
      await api.setCalendarSettings({
        google_client_id: googleId,
        google_client_secret: googleSecret ? googleSecret : null,
        ms_client_id: msId,
      });
      setGoogleSecret("");
      await api.connectCalendar(provider);
      setCalMsg(`${CALENDAR_NAMES[provider] ?? provider} connected.`);
      await loadCal();
      await loadCalList();
    } catch (e) {
      setCalMsg(String(e));
    } finally {
      setCalBusy(null);
    }
  };

  const disconnectCal = async (provider: string) => {
    setCalBusy(provider);
    try {
      await api.disconnectCalendar(provider);
      await loadCal();
      await loadCalList();
      setCalMsg(`${CALENDAR_NAMES[provider] ?? provider} disconnected.`);
    } catch (e) {
      setCalMsg(String(e));
    } finally {
      setCalBusy(null);
    }
  };

  /* ---------------------------------------------------------- derived */

  const asrModels = settings?.models.filter((m) => m.id.startsWith("ggml-")) ?? [];
  // The whisper.cpp engine gets a dedicated, friendlier "Transcription
  // engine" row instead of a cryptic binary entry. Exact-id match: a prefix
  // filter would also hide whisper-bin-vulkan, the Windows GPU build the
  // benchmark manages, which must stay visible in this list.
  const otherModels =
    settings?.models.filter((m) => !m.id.startsWith("ggml-") && m.id !== WHISPER_ENGINE_ID) ?? [];
  const engineInstalling = downloading === WHISPER_ENGINE_ID;
  const engineProg =
    engineInstalling && modelProgress?.id === WHISPER_ENGINE_ID && modelProgress.total > 0
      ? modelProgress
      : null;
  const enginePct = engineProg ? Math.round((engineProg.downloaded / engineProg.total) * 100) : 0;

  const tiles = dataTiles(settings, llm);
  const provider = llm?.providers.find((x) => x.id === llmProvider) ?? null;
  const providerCloud = !!provider && !provider.is_local;
  const cloudProvider = providerCloud ? provider : null;
  const gpuUnavailable = settings?.gpu_available === false;

  /* ---------------------------------------------------------- render: rows */

  const groqKeyBlock = settings && draft && draft.madeOn !== "local" && (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <Input
        type="password"
        label="Groq API key"
        hint="Saved in your keychain."
        autoComplete="off"
        style={MONO_KEY}
        placeholder={settings.has_groq_key ? "Saved. Enter a new key to replace it." : "gsk_…"}
        value={groqKey}
        onChange={(e) => setGroqKey(e.target.value)}
      />
      {!settings.has_groq_key && !groqKey && (
        <p className="text-warning-text" style={SMALL}>
          Without a key, transcripts are made on this computer.
        </p>
      )}
      <p className="text-text-3" style={{ ...SMALL, fontSize: 11.5 }}>
        Who-said-what still runs on this computer. Groq is paced to stay inside its free tier&apos;s
        hourly limit, and finished parts are saved as they complete, so a quit or restart never
        transcribes anything twice.
      </p>
    </div>
  );

  const modelsList = settings && (
    <div>
      {/* Transcription engine — the whisper.cpp binary that runs the weights
          below. Weights alone can't transcribe, so its state is explicit. */}
      <div className={LIST_ROW}>
        <div className="min-w-0">
          <div
            className="truncate text-text"
            style={{ fontFamily: "var(--font-mono)", fontSize: "12.5px" }}
          >
            Transcription engine (whisper.cpp)
          </div>
          <div className="text-text-3" style={{ fontSize: 11, lineHeight: 1.5 }}>
            {settings.engine_installed
              ? "Installed. Runs the models below on this computer."
              : settings.engine_managed
                ? "Required to transcribe. Downloaded models can't run without it."
                : "Required to transcribe. Install whisper.cpp yourself: on macOS, brew install whisper-cpp; on Linux, your package manager or build from source."}
          </div>
          {downloadErrors[WHISPER_ENGINE_ID] && (
            <div style={{ fontSize: 11, color: "var(--error-text)" }}>
              {downloadErrors[WHISPER_ENGINE_ID]}
            </div>
          )}
        </div>
        <span className="flex shrink-0 items-center gap-2">
          {settings.engine_installed ? (
            <Badge tone="success" dot>
              ready
            </Badge>
          ) : engineInstalling ? (
            engineProg ? (
              <span className="flex items-center gap-2">
                <ProgressBar value={enginePct} style={{ width: 96 }} />
                <span className="text-text-3" style={MONO_SMALL}>
                  {enginePct}%
                </span>
              </span>
            ) : (
              <span className="text-text-3" style={MONO_SMALL}>
                downloading…
              </span>
            )
          ) : settings.engine_managed ? (
            <Button variant="outline" size="sm" onClick={() => void download(WHISPER_ENGINE_ID)}>
              Install
            </Button>
          ) : (
            <Badge tone="warning">missing</Badge>
          )}
        </span>
      </div>
      {[...asrModels, ...otherModels].map((m) => {
        const prog =
          downloading === m.id &&
          modelProgress &&
          modelProgress.id === m.id &&
          modelProgress.total > 0
            ? modelProgress
            : null;
        const pct = prog ? Math.round((prog.downloaded / prog.total) * 100) : 0;
        return (
          <div key={m.id} className={LIST_ROW}>
            <div className="min-w-0">
              <div
                className="truncate text-text"
                style={{ fontFamily: "var(--font-mono)", fontSize: "12.5px" }}
              >
                {m.display}
              </div>
              <div className="text-text-3" style={{ fontSize: 11 }}>
                {gb(m.bytes)}
              </div>
              {downloadErrors[m.id] && (
                <div style={{ fontSize: 11, color: "var(--error-text)" }}>
                  {downloadErrors[m.id]}
                </div>
              )}
            </div>
            <span className="flex shrink-0 items-center gap-2">
              {m.installed ? (
                <Badge tone="success" dot>
                  installed
                </Badge>
              ) : downloading === m.id ? (
                prog ? (
                  <span className="flex items-center gap-2">
                    <ProgressBar value={pct} style={{ width: 96 }} />
                    <span className="text-text-3" style={MONO_SMALL}>
                      {pct}%
                    </span>
                  </span>
                ) : (
                  <span className="text-text-3" style={MONO_SMALL}>
                    downloading…
                  </span>
                )
              ) : (
                <Button variant="outline" size="sm" onClick={() => void download(m.id)}>
                  Install
                </Button>
              )}
            </span>
          </div>
        );
      })}
    </div>
  );

  const gpuHelper = (() => {
    if (gpuMsg) return gpuMsg;
    if (gpuUnavailable)
      return "Not available on this Mac. GPU transcription needs Apple Silicon, so this computer uses the CPU.";
    if (!draft?.useGpu) return "Off. Transcripts are made on the CPU.";
    const bench = settings?.gpu_bench;
    if (!bench) return "A one-time speed test runs at the start of the next transcription.";
    if (bench.verdict === "gpu")
      return `Speed test: GPU ${bench.gpu_secs?.toFixed(0)} s, CPU ${bench.cpu_secs?.toFixed(0)} s. Transcribing on the GPU.`;
    if (bench.gpu_secs != null && bench.cpu_secs != null)
      return `Speed test: GPU ${bench.gpu_secs.toFixed(0)} s, CPU ${bench.cpu_secs.toFixed(0)} s. The CPU is faster here, so it stays on the CPU.`;
    return "GPU transcription failed on this computer, so it stays on the CPU. Test again to retry.";
  })();
  const showGpuTestAgain =
    !gpuUnavailable && !!settings?.use_gpu && !!settings.gpu_bench && !!draft?.useGpu;

  /* Ollama (local provider) */
  const effOllamaModel = (llmModel || provider?.default_model || "llama3.1").trim();
  const ollamaModelReady = !!ollama?.models.some(
    (m) => m.name === effOllamaModel || m.name.split(":")[0] === effOllamaModel.split(":")[0],
  );
  // Progress for the runtime install or the model pull.
  const ollamaProg =
    modelProgress &&
    (modelProgress.id === "ollama-bin" || modelProgress.id === `ollama:${effOllamaModel}`) &&
    ollamaBusy !== null &&
    modelProgress.total > 0
      ? modelProgress
      : null;
  const ollamaPct = ollamaProg
    ? Math.round((ollamaProg.downloaded / ollamaProg.total) * 100)
    : null;
  const ollamaServer = !ollama
    ? "Checking Ollama…"
    : ollama.running
      ? ollama.managed
        ? "Server running (managed by this app)."
        : "Server running."
      : ollama.installed
        ? "Server off. It starts automatically when needed."
        : "Not installed.";
  const localModelHelper = !ollama
    ? "Checking Ollama…"
    : !ollama.installed
      ? ollama.can_install
        ? "Ollama isn't installed yet. It runs AI on this computer, with no API key."
        : "Install Ollama from ollama.com, then press Test."
      : ollamaModelReady
        ? `${effOllamaModel} is ready.`
        : ollama.running
          ? `${effOllamaModel} isn't downloaded yet. Ask and Enhance need it.`
          : "Ollama starts automatically when needed.";

  const calendarRow = (prov: "google" | "msgraph") => {
    const connected = prov === "google" ? !!cal?.google_connected : !!cal?.ms_connected;
    const configured = prov === "google" ? !!cal?.google_configured : !!cal?.ms_configured;
    const list = calList.filter((c) => c.provider === prov);
    return (
      <SettingsRow
        key={prov}
        title={CALENDAR_NAMES[prov]}
        helper={
          connected
            ? "Connected"
            : configured
              ? "Not connected"
              : "Add your own OAuth app under More options to connect."
        }
        helperTone={connected ? "success" : "default"}
        control={
          connected ? (
            <Button
              variant="ghost"
              size="sm"
              disabled={calBusy !== null}
              onClick={() => void disconnectCal(prov)}
            >
              Disconnect
            </Button>
          ) : (
            <Button
              variant="outline"
              size="sm"
              disabled={calBusy !== null || !configured}
              onClick={() => void connectCal(prov)}
            >
              {calBusy === prov ? "Waiting for browser…" : "Connect"}
            </Button>
          )
        }
      >
        {list.length > 0 && (
          <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
            <p className="text-text-3" style={{ ...SMALL, fontSize: 11.5, marginBottom: 4 }}>
              Show in Up next. Only events with a join link appear there.
            </p>
            {list.map((c) => (
              <div
                key={`${c.provider}-${c.id}`}
                className="flex items-center justify-between gap-3"
                style={{ padding: "4px 0" }}
              >
                <span className="flex min-w-0 items-center gap-1.5">
                  <span className="truncate text-text" style={{ fontSize: 13 }}>
                    {c.name}
                  </span>
                  {c.primary && (
                    <Badge tone="neutral" size="sm">
                      primary
                    </Badge>
                  )}
                </span>
                <Switch
                  label={`Show ${c.name} in Up next`}
                  checked={c.enabled}
                  onChange={(v) => void toggleCalendar(c, v)}
                />
              </div>
            ))}
          </div>
        )}
      </SettingsRow>
    );
  };

  const updatesRow = (() => {
    const title = `Version ${appVersion ?? "…"}`;
    if (!updater.supported) {
      return (
        <SettingsRow
          title={title}
          helper="Auto-update isn't available for this install (deb packages are managed by apt). New versions are published on the GitHub releases page."
        />
      );
    }
    const u = updater;
    const helper =
      u.phase === "checking"
        ? "Checking for updates…"
        : u.phase === "upToDate"
          ? "You're on the latest version."
          : u.phase === "available"
            ? `Version ${u.version} is available.`
            : u.phase === "downloading"
              ? "Downloading the update…"
              : u.phase === "ready"
                ? recordingActive
                  ? "Update downloaded."
                  : "Update downloaded. Restart when you're ready."
                : u.phase === "installing"
                  ? "Installing. Fly on the Wall will restart…"
                  : u.phase === "error"
                    ? (u.error ?? "Couldn't check for updates.")
                    : "Updates install only when you choose.";
    return (
      <SettingsRow
        title={title}
        helper={helper}
        helperTone={u.phase === "upToDate" ? "success" : u.phase === "error" ? "error" : "default"}
        control={
          <>
            {(u.phase === "idle" || u.phase === "upToDate" || u.phase === "error") && (
              <Button variant="outline" size="sm" onClick={u.check}>
                Check now
              </Button>
            )}
            {u.phase === "checking" && (
              <Button variant="outline" size="sm" disabled>
                Checking…
              </Button>
            )}
            {u.phase === "available" && (
              <Button
                variant="primary"
                size="sm"
                disabled={recordingActive}
                onClick={u.downloadAndInstall}
              >
                Download and install v{u.version}
              </Button>
            )}
            {u.phase === "downloading" && (
              <span className="flex items-center gap-2">
                <ProgressBar
                  value={u.progress == null ? null : Math.round(u.progress * 100)}
                  style={{ width: 96 }}
                />
                <span className="text-text-3" style={MONO_SMALL}>
                  {u.progress == null ? "downloading…" : `${Math.round(u.progress * 100)}%`}
                </span>
              </span>
            )}
            {u.phase === "ready" && (
              <Button variant="primary" size="sm" disabled={recordingActive} onClick={u.restart}>
                Restart now
              </Button>
            )}
          </>
        }
      >
        {recordingActive && (u.phase === "available" || u.phase === "ready") && (
          <p className="text-text-2" style={SMALL}>
            Recording in progress. The update waits until you&apos;re done.
          </p>
        )}
      </SettingsRow>
    );
  })();

  const mcpCmd = mcpCommand(mcpJson);

  /* ---------------------------------------------------------- render */

  return (
    <div
      ref={overlayRef}
      className="fly-settings"
      role="dialog"
      aria-modal="true"
      aria-labelledby="fly-settings-title"
    >
      {/* Layers opened from inside Settings (the discard prompt, future
          popovers) sit above it in the Esc stack. */}
      <EscapeLayerScope depth={escDepth}>
        <header className="fly-settings-head">
          <Button
            variant="ghost"
            size="sm"
            startIcon={<ArrowLeft size={15} strokeWidth={1.75} />}
            onClick={requestClose}
          >
            Back
          </Button>
          <h1 id="fly-settings-title" className="fly-settings-title">
            Settings
          </h1>
          <Button variant="primary" size="sm" onClick={requestClose}>
            Done
          </Button>
        </header>

        <div className="fly-settings-frame">
          <nav className="fly-settings-nav" aria-label="Settings sections">
            <input
              ref={searchRef}
              data-autofocus={initialFocus ? undefined : ""}
              type="search"
              className="fly-settings-search"
              placeholder="Search settings"
              aria-label="Search settings"
              value={query}
              onChange={(e) => onQuery(e.target.value)}
              onKeyDown={(e) => {
                // First Esc clears the search; the next one closes Settings.
                if (e.key === "Escape" && query) {
                  e.preventDefault();
                  onQuery("");
                }
              }}
            />
            {visible.map((s) => (
              <button
                key={s.id}
                type="button"
                className="fly-settings-navitem"
                aria-current={active === s.id ? "true" : undefined}
                onClick={() => goSection(s.id)}
              >
                {s.title}
              </button>
            ))}
            {visible.length === 0 && (
              <span className="fly-settings-nomatch">No settings match.</span>
            )}
          </nav>

          <div className="fly-settings-main">
            <div ref={scrollRef} className="fly-settings-scroll" onScroll={onScroll}>
              <div className="fly-settings-content">
                <div className="fly-settings-section">
                  <h2 className="fly-settings-label">Where your data goes</h2>
                  <div className="fly-settings-tiles">
                    {tiles.map((t) => (
                      <button
                        key={t.section}
                        type="button"
                        className="fly-settings-tile"
                        onClick={() => {
                          if (!show(t.section)) {
                            setQuery("");
                            window.requestAnimationFrame(() => goSection(t.section));
                          } else goSection(t.section);
                        }}
                      >
                        <span style={{ fontSize: 12, color: "var(--text-2)" }}>{t.label}</span>
                        <span
                          className="flex items-center"
                          style={{
                            gap: 6,
                            fontSize: 13,
                            fontWeight: 600,
                            color: t.cloud ? "var(--warning-text)" : "var(--success-text)",
                          }}
                        >
                          <span
                            aria-hidden="true"
                            style={{
                              width: 7,
                              height: 7,
                              borderRadius: 999,
                              background: "currentColor",
                              flex: "none",
                            }}
                          />
                          {t.where}
                        </span>
                        {t.note && (
                          <span style={{ fontSize: 11.5, lineHeight: 1.4, color: "var(--text-3)" }}>
                            {t.note}
                          </span>
                        )}
                      </button>
                    ))}
                  </div>
                </div>

                {visible.length === 0 && (
                  <p className="text-text-3" style={{ margin: 0, fontSize: 13 }}>
                    No settings match.
                  </p>
                )}

                <Section id="recording" hidden={!show("recording")} title="Recording">
                  <SettingsGroup>
                    <SettingsRow
                      title="Microphone"
                      helper="Records your side of the call."
                      control={
                        <Select
                          aria-label="Microphone"
                          style={{ width: 240, maxWidth: "100%" }}
                          value={micId}
                          onChange={(e) => {
                            setMicId(e.target.value);
                            void api.setAppSetting("capture.mic_device_id", e.target.value);
                          }}
                        >
                          <option value="">System default</option>
                          {mics.map((m) => (
                            <option key={m.id} value={m.id}>
                              {m.name}
                              {m.is_default ? " (default)" : ""}
                            </option>
                          ))}
                        </Select>
                      }
                    />
                    <SettingsRow
                      title="Transcribe when I stop"
                      helper={autoMsg ?? "Starts the transcript as soon as a recording stops."}
                      helperTone={autoMsg ? "error" : "default"}
                      control={
                        <Switch
                          label="Transcribe when I stop"
                          checked={autoTranscribe}
                          disabled={!settings || asrBusy}
                          onChange={(v) => void setAutoNow(v)}
                        />
                      }
                    />
                    <RemindNoticeRow />
                  </SettingsGroup>
                </Section>

                <Section id="transcripts" hidden={!show("transcripts")} title="Transcripts">
                  {!settings || !draft ? (
                    <SettingsGroup>
                      <SettingsRow title="Loading…" />
                    </SettingsGroup>
                  ) : (
                    <SettingsGroup
                      expanded={!!advOpen.transcripts}
                      onExpandedChange={(v) => setAdv("transcripts", v)}
                      advanced={
                        <>
                          <SettingsRow
                            title="This computer"
                            helperTone="mono"
                            helper={
                              `${settings.hw.cpu_cores} cores · ${settings.hw.ram_gb} GB RAM · ` +
                              (settings.hw.gpu_name
                                ? `${settings.hw.gpu_name} (${Math.round((settings.hw.vram_mb ?? 0) / 1024)} GB VRAM)`
                                : "no NVIDIA GPU") +
                              ` · recommended quality: ${TIER_NAMES[settings.hw.recommended_tier] ?? settings.hw.recommended_tier}`
                            }
                          />
                          {(() => {
                            const eff = effectiveModel(draft, settings.models);
                            return (
                              <SettingsRow
                                title="Model in use"
                                helperTone="mono"
                                helper={
                                  eff.model
                                    ? `${eff.model.display} · ${gb(eff.model.bytes)}${eff.model.installed ? "" : " · downloads on first use"}`
                                    : eff.id
                                }
                              />
                            );
                          })()}
                          <SettingsRow
                            title="Model"
                            helper="Auto follows Quality. A specific model overrides Quality and Maximum quality."
                            control={
                              <Select
                                aria-label="Transcription model"
                                style={{ width: 230, maxWidth: "100%" }}
                                value={draft.modelId}
                                onChange={(e) => patchDraft({ modelId: e.target.value })}
                              >
                                <option value="">Auto (follows Quality)</option>
                                {asrModels.map((m) => (
                                  <option key={m.id} value={m.id}>
                                    {m.display}
                                  </option>
                                ))}
                              </Select>
                            }
                          />
                          <SettingsRow
                            title="Maximum quality"
                            helper="Full large-v3. Slower, and about a 1 GB download."
                            control={
                              <Switch
                                label="Maximum quality"
                                checked={draft.maxQuality}
                                onChange={(v) => patchDraft({ maxQuality: v })}
                              />
                            }
                          />
                          <SettingsRow
                            title="Use the graphics card"
                            helper={gpuHelper}
                            helperTone={gpuMsg ? "error" : "default"}
                            control={
                              <>
                                {showGpuTestAgain && (
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    disabled={asrBusy}
                                    onClick={() => void testGpuAgain()}
                                  >
                                    {gpuBusy ? "Resetting…" : "Test again"}
                                  </Button>
                                )}
                                <Switch
                                  label="Use the graphics card"
                                  checked={!gpuUnavailable && draft.useGpu}
                                  disabled={gpuUnavailable}
                                  onChange={(v) => patchDraft({ useGpu: v })}
                                />
                              </>
                            }
                          />
                          <SettingsRow
                            id="settings-row-engine"
                            className={flash === "engine" ? "fly-settings-flash" : undefined}
                            title="Models on this computer"
                            helper="The speech engine and models this app downloads."
                          >
                            {modelsList}
                          </SettingsRow>
                        </>
                      }
                    >
                      <SettingsRow
                        title="Quality"
                        helper={
                          draft.madeOn === "groq"
                            ? "Not used with Groq: if it needs help, this computer uses its default model (More options)."
                            : qualityHelper(draft.quality, settings.hw.recommended_tier)
                        }
                        control={
                          <Segmented
                            label="Quality"
                            value={draft.quality}
                            disabled={draft.madeOn === "groq"}
                            onChange={(q) => patchDraft({ quality: q })}
                            options={QUALITY_OPTIONS.map((o) =>
                              o.value === settings.hw.recommended_tier
                                ? { ...o, title: "Recommended for this computer" }
                                : o,
                            )}
                          />
                        }
                      />
                      <SettingsRow
                        id="settings-row-groq"
                        className={flash === "groq" ? "fly-settings-flash" : undefined}
                        title="Made on"
                        badge={
                          draft.madeOn !== "local" && (
                            <Badge tone="warning" size="md">
                              Audio leaves this computer
                            </Badge>
                          )
                        }
                        helper={madeOnHelper(draft.madeOn)}
                        control={
                          <Segmented
                            label="Made on"
                            value={draft.madeOn}
                            onChange={(m) => patchDraft({ madeOn: m })}
                            options={MADE_ON_OPTIONS}
                          />
                        }
                      >
                        {groqKeyBlock}
                      </SettingsRow>
                    </SettingsGroup>
                  )}
                </Section>

                <Section id="ai" hidden={!show("ai")} title="Ask and Enhance">
                  {
                    <SettingsGroup
                      expanded={!!advOpen.ai}
                      onExpandedChange={(v) => setAdv("ai", v)}
                      advanced={
                        <>
                          <SettingsRow
                            title="Endpoint and model"
                            helper="Leave blank for the defaults. Custom OpenAI-compatible servers go here."
                          >
                            <div className="grid grid-cols-2 gap-2">
                              <Input
                                aria-label="Model name"
                                placeholder={`Model (default: ${provider?.default_model ?? ""})`}
                                value={llmModel}
                                onChange={(e) => setLlmModel(e.target.value)}
                              />
                              <Input
                                aria-label="Base URL"
                                placeholder={
                                  llmProvider === "ollama"
                                    ? "Base URL (default: localhost:11434)"
                                    : "Base URL (optional)"
                                }
                                value={llmBaseUrl}
                                onChange={(e) => setLlmBaseUrl(e.target.value)}
                              />
                            </div>
                            {provider &&
                              serverChangeNeedsKey(
                                provider.id,
                                provider.has_key,
                                provider.base_url,
                                llmBaseUrl,
                              ) &&
                              !llmKey && (
                                <p className="mt-2 text-warning-text" style={SMALL}>
                                  Your saved key only goes to the server it was entered for. To use
                                  this one, enter the {providerVendor(provider.id)} API key again,
                                  then save.
                                </p>
                              )}
                            <div className="mt-2 flex items-center gap-2">
                              <Button
                                variant="primary"
                                size="sm"
                                disabled={llmBusy}
                                onClick={() => void saveLlm()}
                              >
                                Save
                              </Button>
                            </div>
                          </SettingsRow>
                          {provider?.is_local && (
                            <SettingsRow title="Local models" helper={ollamaServer}>
                              {ollama && ollama.running && ollama.models.length > 0 && (
                                <div>
                                  {ollama.models.map((m) => {
                                    const inUse =
                                      m.name === effOllamaModel ||
                                      m.name.split(":")[0] === effOllamaModel.split(":")[0];
                                    return (
                                      <div key={m.name} className={LIST_ROW}>
                                        <span className="flex min-w-0 items-center gap-2">
                                          <span
                                            className="truncate text-text"
                                            style={{
                                              fontFamily: "var(--font-mono)",
                                              fontSize: 12.5,
                                            }}
                                          >
                                            {m.name}
                                          </span>
                                          <span className="text-text-3" style={{ fontSize: 11 }}>
                                            {gb(m.size)}
                                          </span>
                                        </span>
                                        {inUse ? (
                                          <Badge tone="neutral" size="sm">
                                            in use
                                          </Badge>
                                        ) : (
                                          <Button
                                            variant="ghost"
                                            size="xs"
                                            disabled={ollamaBusy !== null}
                                            title="Remove this model from disk"
                                            onClick={() => void deleteOllamaModel(m.name)}
                                          >
                                            Delete
                                          </Button>
                                        )}
                                      </div>
                                    );
                                  })}
                                </div>
                              )}
                            </SettingsRow>
                          )}
                        </>
                      }
                    >
                      <SettingsRow
                        id="settings-row-llm"
                        className={flash === "llm-key" ? "fly-settings-flash" : undefined}
                        title="Runs on"
                        badge={
                          providerCloud && (
                            <Badge tone="warning" size="md">
                              Text leaves this computer
                            </Badge>
                          )
                        }
                        helper={
                          !llm
                            ? (llmLoadError ?? "Loading…")
                            : !provider
                              ? `Unknown provider “${llmProvider}”. Pick one to continue.`
                              : providerCloud
                                ? `Notes and transcripts go to ${providerVendor(provider.id)} for Ask, Enhance, transcript cleanup and action items. Audio stays here, and so do notes you keep local.`
                                : "Nothing leaves this computer. Slower on long meetings."
                        }
                        helperTone={llm ? "default" : llmLoadError ? "error" : "default"}
                        control={
                          llm && (
                            <>
                              <Select
                                aria-label="Runs on"
                                disabled={llmBusy}
                                style={{ width: 210, maxWidth: "100%" }}
                                value={llmProvider}
                                onChange={(e) => void pickProvider(e.target.value)}
                              >
                                {!provider && <option value={llmProvider}>{llmProvider}</option>}
                                {llm.providers.map((p) => (
                                  <option key={p.id} value={p.id}>
                                    {providerLabel(p.id)}
                                  </option>
                                ))}
                              </Select>
                              <Button
                                variant="outline"
                                size="sm"
                                disabled={llmBusy}
                                onClick={() => void testLlm()}
                              >
                                Test
                              </Button>
                            </>
                          )
                        }
                      >
                        {(cloudProvider || llmTest) && (
                          <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                            {cloudProvider && (
                              <div className="flex flex-wrap items-end gap-2">
                                <div style={{ flex: "1 1 240px", minWidth: 0 }}>
                                  <Input
                                    type="password"
                                    label={`${providerVendor(cloudProvider.id)} API key`}
                                    hint="Saved in your keychain."
                                    autoComplete="off"
                                    style={MONO_KEY}
                                    placeholder={
                                      cloudProvider.has_key
                                        ? "Saved. Enter a new key to replace it."
                                        : `Paste your ${providerVendor(cloudProvider.id)} API key`
                                    }
                                    value={llmKey}
                                    onChange={(e) => setLlmKey(e.target.value)}
                                  />
                                </div>
                                <div style={{ paddingBottom: 22 }}>
                                  <Button
                                    variant="primary"
                                    size="md"
                                    disabled={llmBusy || !llmKey}
                                    onClick={() => void saveLlm()}
                                  >
                                    {cloudProvider.has_key ? "Update" : "Save"}
                                  </Button>
                                </div>
                              </div>
                            )}
                            {cloudProvider && !cloudProvider.has_key && !llmKey && (
                              <p className="text-warning-text" style={SMALL}>
                                No key saved yet. Ask, Enhance and cleanup won&apos;t work until you
                                add one.
                              </p>
                            )}
                            {llmTest && (
                              <p role="status" className="text-text-2" style={SMALL}>
                                {llmTest}
                              </p>
                            )}
                          </div>
                        )}
                      </SettingsRow>

                      {llmProvider === "anthropic" && (
                        <SettingsRow
                          title="Model"
                          helper="The Claude model used for Ask, Enhance and cleanup."
                          control={
                            <Select
                              aria-label="Claude model"
                              disabled={llmBusy}
                              style={{ width: 260, maxWidth: "100%" }}
                              value={
                                llmModel || provider?.default_model || ANTHROPIC_MODELS[0].value
                              }
                              onChange={(e) => void saveClaudeModel(e.target.value)}
                            >
                              {ANTHROPIC_MODELS.map((m) => (
                                <option key={m.value} value={m.value}>
                                  {m.label}
                                </option>
                              ))}
                              {llmModel && !ANTHROPIC_MODELS.some((m) => m.value === llmModel) && (
                                <option value={llmModel}>Custom: {llmModel}</option>
                              )}
                            </Select>
                          }
                        />
                      )}

                      {provider?.is_local && (
                        <SettingsRow
                          title="Local model"
                          helper={localModelHelper}
                          helperTone={
                            ollama?.running && ollamaModelReady
                              ? "success"
                              : ollama?.running && !ollamaModelReady
                                ? "warning"
                                : "default"
                          }
                          control={
                            <>
                              {ollama && !ollama.installed && ollama.can_install && (
                                <Button
                                  variant="primary"
                                  size="sm"
                                  disabled={ollamaBusy !== null}
                                  onClick={() => void installOllama()}
                                >
                                  {ollamaBusy === "install"
                                    ? "Installing…"
                                    : "Install Ollama (1.5 GB download)"}
                                </Button>
                              )}
                              {ollama && ollama.installed && !ollamaModelReady && (
                                <Button
                                  variant="primary"
                                  size="sm"
                                  disabled={ollamaBusy !== null}
                                  onClick={() => void pullOllamaModel(effOllamaModel)}
                                >
                                  {ollamaBusy === "pull"
                                    ? "Downloading…"
                                    : `Download ${effOllamaModel}`}
                                </Button>
                              )}
                            </>
                          }
                        >
                          {(ollamaPct !== null || ollamaMsg || swapOffer || deleteOffer) && (
                            <div className="space-y-2">
                              {ollamaPct !== null && (
                                <div className="flex items-center gap-2">
                                  <ProgressBar value={ollamaPct} style={{ width: 180 }} />
                                  <span className="text-text-3" style={{ fontSize: 11 }}>
                                    {ollamaPct}%
                                  </span>
                                </div>
                              )}
                              {ollamaMsg && (
                                <p role="status" className="text-text-2" style={SMALL}>
                                  {ollamaMsg}
                                </p>
                              )}
                              {swapOffer && !ollamaModelReady && (
                                <div className="flex flex-wrap items-center gap-2">
                                  <span className="text-text-2" style={{ fontSize: 12 }}>
                                    “{swapOffer.to}” isn’t downloaded yet. Enhance and Ask would
                                    fail until it is.
                                  </span>
                                  <Button
                                    variant="primary"
                                    size="sm"
                                    disabled={ollamaBusy !== null}
                                    onClick={() => void pullOllamaModel(swapOffer.to)}
                                  >
                                    {ollamaBusy === "pull"
                                      ? "Downloading…"
                                      : `Download ${swapOffer.to}`}
                                  </Button>
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    onClick={() => setSwapOffer(null)}
                                  >
                                    Later
                                  </Button>
                                </div>
                              )}
                              {deleteOffer && (
                                <div className="flex flex-wrap items-center gap-2">
                                  <span className="text-text-2" style={{ fontSize: 12 }}>
                                    Switched models. Delete “{deleteOffer.name}” to free{" "}
                                    {gb(deleteOffer.size)}?
                                  </span>
                                  <Button
                                    variant="outline"
                                    size="sm"
                                    onClick={() => {
                                      const d = deleteOffer;
                                      setDeleteOffer(null);
                                      void deleteOllamaModel(d.name);
                                    }}
                                  >
                                    Delete
                                  </Button>
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    onClick={() => setDeleteOffer(null)}
                                  >
                                    Keep
                                  </Button>
                                </div>
                              )}
                            </div>
                          )}
                        </SettingsRow>
                      )}

                      <SettingsRow
                        title="Changes to your notes"
                        helper="Ask reads freely. It only writes when you click Insert."
                        control={
                          <span style={{ fontSize: 13, fontWeight: 600, color: "var(--text-2)" }}>
                            Always asks first
                          </span>
                        }
                      />

                      <SettingsRow
                        title="Note templates"
                        helper="Saved prompts that shape Enhance."
                        control={
                          <Button
                            variant="outline"
                            size="sm"
                            aria-expanded={templatesOpen}
                            // An open edit is saved or cancelled in the editor,
                            // never dropped by collapsing the list.
                            disabled={!!editingTpl}
                            title={editingTpl ? "Save or cancel the template first" : undefined}
                            onClick={() => setTemplatesOpen((o) => !o)}
                          >
                            {templatesOpen ? "Close" : `Manage · ${templates.length}`}
                          </Button>
                        }
                      >
                        {templatesOpen &&
                          (editingTpl ? (
                            <Card tone="muted" pad="md">
                              <div className="space-y-2">
                                <Input
                                  aria-label="Template name"
                                  value={editingTpl.name}
                                  onChange={(e) =>
                                    setEditingTpl({ ...editingTpl, name: e.target.value })
                                  }
                                  placeholder="Template name"
                                />
                                <textarea
                                  aria-label="System prompt"
                                  value={editingTpl.system_prompt}
                                  onChange={(e) =>
                                    setEditingTpl({ ...editingTpl, system_prompt: e.target.value })
                                  }
                                  rows={3}
                                  style={TEXTAREA}
                                  placeholder="System prompt"
                                />
                                <textarea
                                  aria-label="Structure hint"
                                  value={editingTpl.structure_hint}
                                  onChange={(e) =>
                                    setEditingTpl({
                                      ...editingTpl,
                                      structure_hint: e.target.value,
                                    })
                                  }
                                  rows={3}
                                  style={{ ...TEXTAREA, fontFamily: "var(--font-mono)" }}
                                  placeholder="Structure hint (markdown headings)"
                                />
                                <div className="flex gap-2">
                                  <Button
                                    variant="primary"
                                    size="sm"
                                    onClick={() => void saveTpl()}
                                  >
                                    Save template
                                  </Button>
                                  <Button
                                    variant="outline"
                                    size="sm"
                                    onClick={() => setEditingTpl(null)}
                                  >
                                    Cancel
                                  </Button>
                                </div>
                              </div>
                            </Card>
                          ) : (
                            <div>
                              {templates.map((t) => (
                                <div key={t.id} className={LIST_ROW} style={{ fontSize: 13 }}>
                                  <span className="flex min-w-0 items-center gap-2">
                                    <span className="truncate text-text">{t.name}</span>
                                    {t.built_in && (
                                      <Badge tone="neutral" size="sm">
                                        built-in
                                      </Badge>
                                    )}
                                  </span>
                                  <span className="flex flex-none items-center gap-1.5">
                                    <Button
                                      variant="ghost"
                                      size="sm"
                                      onClick={() => setEditingTpl(t)}
                                    >
                                      Edit
                                    </Button>
                                    <Button
                                      variant="outline"
                                      size="sm"
                                      className="hover:text-rec"
                                      onClick={() => void removeTpl(t.id)}
                                    >
                                      Delete
                                    </Button>
                                  </span>
                                </div>
                              ))}
                              <div style={{ marginTop: 8 }}>
                                <Button
                                  variant="outline"
                                  size="sm"
                                  startIcon={<Plus size={14} strokeWidth={1.75} />}
                                  onClick={() =>
                                    setEditingTpl({
                                      id: "",
                                      name: "New template",
                                      system_prompt:
                                        "You are an expert meeting-notes editor. Never invent facts.",
                                      structure_hint: "## Summary\n## Action items",
                                      built_in: false,
                                    })
                                  }
                                >
                                  New template
                                </Button>
                              </div>
                            </div>
                          ))}
                      </SettingsRow>
                    </SettingsGroup>
                  }
                </Section>

                <Section id="calendars" hidden={!show("calendars")} title="Calendars">
                  <SettingsGroup
                    expanded={!!advOpen.calendars}
                    onExpandedChange={(v) => setAdv("calendars", v)}
                    advanced={
                      <SettingsRow
                        title="Your own OAuth app"
                        helper="Optional. A Google Cloud “Desktop app” client (ID and secret) and/or an Azure app registration with a public client and loopback redirect. Sign-ins are kept in your system keychain; the README has a step-by-step."
                      >
                        <div className="grid grid-cols-2 gap-2">
                          <Input
                            aria-label="Google client ID"
                            style={MONO_KEY}
                            placeholder="Google client ID (….apps.googleusercontent.com)"
                            value={googleId}
                            onChange={(e) => setGoogleId(e.target.value)}
                          />
                          <Input
                            aria-label="Google client secret"
                            type="password"
                            autoComplete="off"
                            style={MONO_KEY}
                            placeholder={
                              cal?.google_has_secret
                                ? "Google secret saved. Enter to replace."
                                : "Google client secret"
                            }
                            value={googleSecret}
                            onChange={(e) => setGoogleSecret(e.target.value)}
                          />
                          <div className="col-span-2">
                            <Input
                              aria-label="Outlook application (client) ID"
                              style={MONO_KEY}
                              placeholder="Outlook application (client) ID. No secret needed."
                              value={msId}
                              onChange={(e) => setMsId(e.target.value)}
                            />
                          </div>
                        </div>
                        <div className="mt-2">
                          <Button variant="primary" size="sm" onClick={() => void saveCal()}>
                            Save
                          </Button>
                        </div>
                      </SettingsRow>
                    }
                  >
                    {calendarRow("google")}
                    {calendarRow("msgraph")}
                  </SettingsGroup>
                  {calMsg && (
                    <p role="status" className="text-text-2" style={{ ...SMALL, paddingLeft: 4 }}>
                      {calMsg}
                    </p>
                  )}
                </Section>

                <Section id="files" hidden={!show("files")} title="Your files and privacy">
                  <PrivacySection recordingActive={recordingActive}>
                    <SettingsRow
                      title="Where your files are"
                      helperTone="mono"
                      helper={appInfo?.data_dir ?? "…"}
                      control={
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => void api.revealDataDir().catch(console.error)}
                        >
                          Show folder
                        </Button>
                      }
                    />
                    <SettingsRow
                      title="Search index"
                      helper="Pulls decisions and action items out of meetings recorded before this existed, so Ask and other AI apps can find them. Uses your Ask and Enhance provider."
                      control={
                        <Button
                          variant="outline"
                          size="sm"
                          disabled={backfillBusy || backfillPending === 0}
                          onClick={() => void runBackfill()}
                        >
                          {backfillBusy
                            ? "Extracting…"
                            : backfillPending === 0
                              ? "All meetings extracted"
                              : backfillPending === null
                                ? "Extract from older meetings"
                                : `Extract from ${backfillPending} meeting${backfillPending === 1 ? "" : "s"}`}
                        </Button>
                      }
                    >
                      {backfillMsg && (
                        <p role="status" className="text-text-2" style={SMALL}>
                          {backfillMsg}
                          {backfillDetail && (
                            <span
                              className="text-text-3"
                              style={{ display: "block", marginTop: 2 }}
                            >
                              {backfillDetail}
                            </span>
                          )}
                        </p>
                      )}
                    </SettingsRow>
                  </PrivacySection>
                </Section>

                <Section id="apps" hidden={!show("apps")} title="Other AI apps">
                  <SettingsGroup
                    expanded={!!advOpen.apps}
                    onExpandedChange={(v) => setAdv("apps", v)}
                    advanced={
                      <SettingsRow
                        title="Claude Code, Cursor and VS Code"
                        helper="They run the same local server."
                      >
                        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                          {mcpCmd && (
                            <SnippetCard label="Claude Code" text={claudeCodeCommand(mcpCmd)} />
                          )}
                          <p className="text-text-2" style={SMALL}>
                            Cursor: add the Claude Desktop block to{" "}
                            <code style={MONO_SMALL}>~/.cursor/mcp.json</code>. VS Code: add the
                            same server under <code style={MONO_SMALL}>&quot;servers&quot;</code> in{" "}
                            <code style={MONO_SMALL}>.vscode/mcp.json</code> with{" "}
                            <code style={MONO_SMALL}>&quot;type&quot;: &quot;stdio&quot;</code>.
                          </p>
                        </div>
                      </SettingsRow>
                    }
                  >
                    <SettingsRow
                      title="Claude Desktop"
                      helper="Lets Claude search and read your meetings. The server runs on this computer."
                      control={
                        <Button
                          variant="outline"
                          size="sm"
                          aria-expanded={mcpSetupOpen}
                          onClick={() => setMcpSetupOpen((o) => !o)}
                        >
                          {mcpSetupOpen ? "Hide" : "Set up"}
                        </Button>
                      }
                    >
                      {mcpSetupOpen && (
                        <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
                          <ol
                            className="text-text-2"
                            style={{ ...SMALL, paddingLeft: 18, listStyle: "decimal" }}
                          >
                            <li>
                              In Claude Desktop, open Settings › Developer › Edit Config
                              {appInfo?.os === "macos" && (
                                <>
                                  {" "}
                                  (
                                  <code style={MONO_SMALL}>
                                    ~/Library/Application Support/Claude/claude_desktop_config.json
                                  </code>
                                  )
                                </>
                              )}
                              .
                            </li>
                            <li>Add this to the file, then restart Claude Desktop.</li>
                          </ol>
                          {mcpJson ? (
                            <SnippetCard label="claude_desktop_config.json" text={mcpJson} />
                          ) : (
                            <p className="text-text-3" style={SMALL}>
                              Loading the snippet…
                            </p>
                          )}
                          <p className="text-text-3" style={{ ...SMALL, fontSize: 11.5 }}>
                            Fly on the Wall never edits Claude&apos;s files itself.
                          </p>
                        </div>
                      )}
                    </SettingsRow>
                    <SettingsRow
                      title="What it can see"
                      helper="Whatever the server returns goes to the AI model your app uses (for Claude Desktop, Anthropic's cloud) under that app's terms. It can read every note, including notes you keep local."
                    />
                  </SettingsGroup>
                </Section>

                <Section id="general" hidden={!show("general")} title="General">
                  <SettingsGroup
                    expanded={!!advOpen.general}
                    onExpandedChange={(v) => setAdv("general", v)}
                    advanced={
                      <SettingsRow
                        title="Diagnostics"
                        helper="Local log files from the last few days, for troubleshooting. Nothing leaves this computer."
                        control={
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => void api.revealLogsDir()}
                          >
                            Open logs folder
                          </Button>
                        }
                      />
                    }
                  >
                    <SettingsRow
                      title="Appearance"
                      control={
                        <Segmented
                          label="Appearance"
                          value={theme}
                          onChange={(v) => setTheme(v)}
                          options={[
                            { value: "system", label: "System" },
                            { value: "light", label: "Light" },
                            { value: "dark", label: "Dark" },
                          ]}
                        />
                      }
                    />
                    {updatesRow}
                  </SettingsGroup>
                </Section>
              </div>
            </div>

            {asrDirty && (
              <div
                className="fly-settings-savebar"
                role="region"
                aria-label="Unsaved transcript settings"
              >
                <span style={{ flex: 1, minWidth: 200, fontSize: 13 }}>
                  {saveError ? (
                    <span className="text-error-text">Couldn&apos;t save: {saveError}</span>
                  ) : (
                    <span className="text-text">
                      Your transcript changes aren&apos;t saved yet.
                    </span>
                  )}
                </span>
                <Button variant="ghost" size="sm" disabled={asrBusy} onClick={discard}>
                  Discard
                </Button>
                <Button variant="primary" size="sm" disabled={asrBusy} onClick={() => void save()}>
                  {saving ? "Saving…" : "Save changes"}
                </Button>
              </div>
            )}
          </div>
        </div>

        <Modal
          open={confirmClose}
          onClose={() => setConfirmClose(false)}
          title="Save your transcript changes?"
          width={440}
          footer={
            <>
              <Button variant="ghost" size="sm" onClick={() => setConfirmClose(false)}>
                Keep editing
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={asrBusy}
                onClick={() => {
                  discard();
                  setConfirmClose(false);
                  onClose();
                }}
              >
                Discard
              </Button>
              <Button
                variant="primary"
                size="sm"
                disabled={asrBusy}
                onClick={() =>
                  void save().then((ok) => {
                    setConfirmClose(false);
                    if (ok) onClose();
                  })
                }
              >
                {saving ? "Saving…" : "Save"}
              </Button>
            </>
          }
        >
          <p className="text-text-2" style={{ margin: 0, fontSize: 13.5, lineHeight: 1.55 }}>
            Quality, Made on, model and graphics-card changes only apply when you save them.
          </p>
        </Modal>
      </EscapeLayerScope>
    </div>
  );
}
