import type { CSSProperties, ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { Check, Copy, Plus } from "lucide-react";
import { api } from "../api";
import { WHISPER_ENGINE_ID } from "../types";
import type {
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
  Checkbox,
  Input,
  Modal,
  ProgressBar,
  SectionLabel,
  Select,
} from "./ui";
import { useTheme } from "../theme";
import type { Updater } from "../updater";

interface Props {
  modelProgress: ModelProgress | null;
  updater: Updater;
  recordingActive: boolean;
  appVersion: string | null;
  /** Deep-link from the transcribe error: "engine" opens Technical view
   *  scrolled to the transcription-engine row; "groq" opens Technical view
   *  scrolled to the Groq card with the enable checkbox visible. */
  initialFocus?: "engine" | "groq" | null;
  onClose: () => void;
}

const TIERS = [
  { id: "light", label: "Light", desc: "Whisper small (Q5) — ~2 GB RAM, fine on old laptops" },
  { id: "balanced", label: "Balanced", desc: "large-v3-turbo (Q5) — near-large accuracy on CPU" },
  { id: "best", label: "Best", desc: "large-v3-turbo (Q5), large-v3 with max-quality toggle" },
  { id: "cloud", label: "Cloud (Groq)", desc: "For weak devices — audio LEAVES this machine" },
];

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
const MONO_KEY: CSSProperties = { fontFamily: "var(--font-mono)", fontSize: "12px" };
/* "LEAVES this machine" emphasis pill in the cloud-tier tooltip/warning. */
const LEAVES_PILL: CSSProperties = {
  display: "inline-block",
  background: "var(--warning-soft)",
  color: "var(--warning-text)",
  fontWeight: 700,
  fontSize: "10px",
  letterSpacing: "0.04em",
  padding: "0 6px",
  borderRadius: "var(--radius-pill)",
  verticalAlign: "baseline",
};

function gb(bytes: number): string {
  return bytes >= 1_000_000_000
    ? `${(bytes / 1_000_000_000).toFixed(1)} GB`
    : `${Math.round(bytes / 1_000_000)} MB`;
}

/* ------------------------------------------------------------------ Segmented
   Composed pill control (Simple/Technical, tiers, appearance, provider).
   Built inline per the design template's <Segmented> — no shipped primitive. */
type SegOption = [string, ReactNode, ReactNode?];

function Segmented({
  value,
  onChange,
  options,
  full = false,
}: {
  value: string;
  onChange: (v: string) => void;
  options: SegOption[];
  full?: boolean;
}) {
  const [hint, setHint] = useState<string | null>(null);
  const last = options.length - 1;
  return (
    <div
      style={{
        display: full ? "flex" : "inline-flex",
        width: full ? "100%" : undefined,
        padding: 3,
        gap: 3,
        flex: "none",
        borderRadius: "var(--radius-pill)",
        border: "1px solid var(--line)",
        background: "var(--surface-2)",
      }}
    >
      {options.map(([id, l, tip], i) => {
        const on = value === id;
        // keep edge tooltips inside the panel: first anchors left, last right, middle centers
        const anchor: CSSProperties =
          i === 0
            ? { left: 0 }
            : i === last
              ? { right: 0 }
              : { left: "50%", transform: "translateX(-50%)" };
        return (
          <button
            key={id}
            type="button"
            onClick={() => onChange(id)}
            onMouseEnter={tip ? () => setHint(id) : undefined}
            onMouseLeave={tip ? () => setHint((h) => (h === id ? null : h)) : undefined}
            style={{
              position: "relative",
              flex: full ? 1 : undefined,
              border: "none",
              cursor: "pointer",
              borderRadius: "var(--radius-pill)",
              padding: "5px 13px",
              fontSize: "12.5px",
              fontWeight: 600,
              fontFamily: "var(--font-sans)",
              whiteSpace: "nowrap",
              background: on ? "var(--primary)" : "transparent",
              color: on ? "var(--on-primary)" : "var(--text-2)",
              transition: "background .12s, color .12s",
            }}
          >
            {l}
            {tip && hint === id && (
              <span
                role="tooltip"
                style={{
                  position: "absolute",
                  bottom: "calc(100% + 8px)",
                  ...anchor,
                  zIndex: 20,
                  width: 200,
                  maxWidth: "60vw",
                  whiteSpace: "normal",
                  textAlign: "left",
                  background: "var(--text)",
                  color: "var(--surface)",
                  fontSize: "11.5px",
                  fontWeight: 500,
                  lineHeight: 1.45,
                  padding: "7px 10px",
                  borderRadius: 8,
                  boxShadow: "var(--shadow-lg)",
                  pointerEvents: "none",
                }}
              >
                {tip}
              </span>
            )}
          </button>
        );
      })}
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
  const [settings, setSettings] = useState<AsrSettings | null>(null);
  const [tier, setTier] = useState("balanced");
  const [modelId, setModelId] = useState<string>("");
  const [useGroq, setUseGroq] = useState(false);
  const [maxQuality, setMaxQuality] = useState(false);
  const [autoTranscribe, setAutoTranscribe] = useState(true);
  const [useGpu, setUseGpu] = useState(true);
  const [groqKey, setGroqKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [downloadErrors, setDownloadErrors] = useState<Record<string, string>>({});
  const [backfillBusy, setBackfillBusy] = useState(false);
  const [backfillMsg, setBackfillMsg] = useState<string | null>(null);

  // Presentational view mode — Technical reveals model installs, GPU + OAuth details.
  // A deep-link forces Technical so its target (engine row, or the Groq
  // enable checkbox under the Groq card) is actually rendered.
  const [technical, setTechnical] = useState(initialFocus !== null);
  const engineRowRef = useRef<HTMLDivElement>(null);
  const groqCardRef = useRef<HTMLDivElement>(null);
  const didFocusRef = useRef(false);
  // Deep-link: once settings have loaded (target is rendered), scroll it into
  // view a single time — not on every later refresh (e.g. post-install).
  useEffect(() => {
    if (initialFocus && settings && !didFocusRef.current) {
      didFocusRef.current = true;
      const target = initialFocus === "groq" ? groqCardRef : engineRowRef;
      target.current?.scrollIntoView({ block: "center" });
    }
  }, [initialFocus, settings]);
  // Appearance control (System / Light / Dark) — wired to the shared theme hook.
  const { theme, setTheme } = useTheme();

  // LLM provider state
  const [llm, setLlm] = useState<LlmSettings | null>(null);
  const [llmProvider, setLlmProvider] = useState("ollama");
  const [llmModel, setLlmModel] = useState("");
  const [llmBaseUrl, setLlmBaseUrl] = useState("");
  const [llmKey, setLlmKey] = useState("");
  const [llmTest, setLlmTest] = useState<string | null>(null);
  const [llmBusy, setLlmBusy] = useState(false);

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

  // MCP state
  const [mcpJson, setMcpJson] = useState("");
  const [mcpCopied, setMcpCopied] = useState(false);

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
    await api.setCalendarSettings({
      google_client_id: googleId,
      google_client_secret: googleSecret ? googleSecret : null,
      ms_client_id: msId,
    });
    setGoogleSecret("");
    await loadCal();
    setCalMsg("saved ✓");
  };

  const connectCal = async (provider: string) => {
    setCalBusy(provider);
    setCalMsg("A browser window opened — finish signing in there…");
    try {
      await api.setCalendarSettings({
        google_client_id: googleId,
        google_client_secret: googleSecret ? googleSecret : null,
        ms_client_id: msId,
      });
      setGoogleSecret("");
      await api.connectCalendar(provider);
      setCalMsg(`${provider} connected ✓`);
      await loadCal();
      await loadCalList();
    } catch (e) {
      setCalMsg(`✗ ${e}`);
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
      setCalMsg(`${provider} disconnected`);
    } catch (e) {
      setCalMsg(String(e));
    } finally {
      setCalBusy(null);
    }
  };

  const load = () =>
    api.getAsrSettings().then((s) => {
      setSettings(s);
      setTier(s.tier);
      setModelId(s.model_id ?? "");
      setUseGroq(s.use_groq);
      setMaxQuality(s.max_quality);
      setAutoTranscribe(s.auto_transcribe);
      setUseGpu(s.use_gpu);
    });

  const loadLlm = () =>
    api.getLlmSettings().then((s) => {
      setLlm(s);
      setLlmProvider(s.provider);
      const info = s.providers.find((p) => p.id === s.provider);
      setLlmModel(info?.model ?? "");
      setLlmBaseUrl(info?.base_url ?? "");
    });

  const loadOllama = () => api.ollamaStatus().then(setOllama);

  const installOllama = async () => {
    setOllamaBusy("install");
    setOllamaMsg(null);
    try {
      await api.downloadModel("ollama-bin");
      setOllamaMsg("installed ✓");
    } catch (e) {
      setOllamaMsg(`✗ ${e}`);
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
      setOllamaMsg(`${model} ready ✓`);
      // A guided swap just finished downloading its target: close the offer
      // and, if the previous model is still on disk, offer to reclaim it.
      if (swapOffer && sameOllamaModel(model, swapOffer.to)) {
        const from = swapOffer.from;
        setSwapOffer(null);
        const old = from ? installedOllamaModel(from) : null;
        if (old && !sameOllamaModel(old.name, model)) setDeleteOffer(old);
      }
    } catch (e) {
      setOllamaMsg(`✗ ${e}`);
    } finally {
      setOllamaBusy(null);
      loadOllama().catch(console.error);
    }
  };

  const deleteOllamaModel = async (model: string) => {
    if (!confirm(`Delete model "${model}" from this machine?`)) return;
    setOllamaMsg(null);
    try {
      await api.ollamaDelete(model);
      setOllamaMsg(`${model} deleted`);
      setDeleteOffer((d) => (d && sameOllamaModel(d.name, model) ? null : d));
    } catch (e) {
      setOllamaMsg(`✗ ${e}`);
    } finally {
      loadOllama().catch(console.error);
    }
  };

  useEffect(() => {
    load().catch(console.error);
    loadLlm().catch(console.error);
    loadOllama().catch(console.error);
    api.listTemplates().then(setTemplates).catch(console.error);
    loadCal().catch(console.error);
    loadCalList().catch(console.error);
    api.mcpConfig().then(setMcpJson).catch(console.error);
    api.listMicDevices().then(setMics).catch(console.error);
    api
      .getAppSetting("capture.mic_device_id")
      .then((v) => setMicId(v ?? ""))
      .catch(console.error);
  }, []);

  const pickProvider = (id: string) => {
    const prev = llmProvider;
    setLlmProvider(id);
    const info = llm?.providers.find((p) => p.id === id);
    setLlmModel(info?.model ?? "");
    setLlmBaseUrl(info?.base_url ?? "");
    setLlmKey("");
    setLlmTest(null);
    // Persist the pick immediately (optimistic, like toggleCalendar). The
    // Save button is far below the Ollama panel, so users routinely picked a
    // provider, never hit Save, and reopened the modal to persisted Ollama.
    // Save still owns model / base-url / key edits.
    api
      .setLlmSettings({
        provider: id,
        model: info?.model ?? null,
        base_url: info?.base_url ?? null,
        api_key: null,
      })
      .catch((e) => {
        setLlmProvider(prev);
        setLlmTest(String(e));
      });
  };

  const saveLlm = async () => {
    setLlmBusy(true);
    setLlmTest(null);
    // The Ollama model that was saved BEFORE this save — the "old" side of a
    // guided swap (llm state is reloaded below).
    const ollamaInfo = llm?.providers.find((p) => p.id === "ollama");
    const prevModel = (ollamaInfo?.model ?? ollamaInfo?.default_model ?? "").trim() || null;
    try {
      await api.setLlmSettings({
        provider: llmProvider,
        model: llmModel || null,
        base_url: llmBaseUrl || null,
        api_key: llmKey ? llmKey : null,
      });
      setLlmKey("");
      await loadLlm();
      setLlmTest("saved ✓");
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

  const testLlm = async () => {
    setLlmBusy(true);
    setLlmTest("testing…");
    try {
      await api.setLlmSettings({
        provider: llmProvider,
        model: llmModel || null,
        base_url: llmBaseUrl || null,
        api_key: llmKey ? llmKey : null,
      });
      setLlmKey("");
      setLlmTest(await api.testLlmConnection());
      await loadLlm();
    } catch (e) {
      setLlmTest(`✗ ${e}`);
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

  const save = async () => {
    setSaving(true);
    try {
      await api.setAsrSettings({
        tier,
        model_id: modelId || null,
        use_groq: useGroq,
        max_quality: maxQuality,
        auto_transcribe: autoTranscribe,
        use_gpu: useGpu,
        groq_key: groqKey ? groqKey : null,
      });
      onClose();
    } finally {
      setSaving(false);
    }
  };

  const runBackfill = async () => {
    setBackfillBusy(true);
    setBackfillMsg(null);
    try {
      const r = await api.backfillMeetingItems();
      setBackfillMsg(
        r.processed === 0
          ? "All meetings are already extracted."
          : `${r.processed} meeting(s) processed, ${r.extracted} item(s) extracted` +
              (r.failed ? `, ${r.failed} failed` : ""),
      );
    } catch (e) {
      setBackfillMsg(String(e));
    } finally {
      setBackfillBusy(false);
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
      await load();
    } catch (e) {
      console.error(e);
      setDownloadErrors((prev) => ({ ...prev, [id]: String(e) }));
    } finally {
      setDownloading(null);
    }
  };

  const asrModels = settings?.models.filter((m) => m.id.startsWith("ggml-")) ?? [];
  // The whisper.cpp engine gets a dedicated, friendlier "Transcription
  // engine" row below instead of a cryptic binary entry here. Exact-id match:
  // a prefix filter would also hide whisper-bin-vulkan, the Windows GPU
  // build the benchmark manages, which must stay visible in this list.
  const otherModels =
    settings?.models.filter((m) => !m.id.startsWith("ggml-") && m.id !== WHISPER_ENGINE_ID) ?? [];
  const engineInstalling = downloading === WHISPER_ENGINE_ID;
  const engineProg =
    engineInstalling && modelProgress?.id === WHISPER_ENGINE_ID && modelProgress.total > 0
      ? modelProgress
      : null;
  const enginePct = engineProg ? Math.round((engineProg.downloaded / engineProg.total) * 100) : 0;

  const groqHasKey = !!settings?.has_groq_key || !!groqKey;
  // The Groq deep-link must land somewhere the user can actually enable it,
  // so it renders the card even before the toggle/tier is on.
  const showGroqCard = tier === "cloud" || useGroq || initialFocus === "groq";

  return (
    <Modal
      open
      onClose={onClose}
      title="Settings"
      width={640}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={saving} onClick={() => void save()}>
            {saving ? "Saving…" : "Save"}
          </Button>
        </>
      }
    >
      <div style={{ display: "flex", flexDirection: "column", gap: 22 }}>
        {/* View mode — Simple hides model installs / GPU / OAuth details */}
        <section className="flex items-center justify-between gap-3">
          <div>
            <SectionLabel style={{ display: "block", marginBottom: 3 }}>View</SectionLabel>
            <span className="text-text-3" style={{ fontSize: "11.5px" }}>
              Technical adds model installs &amp; OAuth credentials.
            </span>
          </div>
          <Segmented
            value={technical ? "technical" : "simple"}
            onChange={(v) => setTechnical(v === "technical")}
            options={[
              ["simple", "Simple"],
              ["technical", "Technical"],
            ]}
          />
        </section>

        {/* Machine info */}
        {settings && (
          <div
            className="bg-bg text-text-2"
            style={{ borderRadius: 12, padding: "10px 14px", fontSize: 12 }}
          >
            This machine: {settings.hw.cpu_cores} cores · {settings.hw.ram_gb} GB RAM — recommended
            tier: <b className="capitalize text-text">{settings.hw.recommended_tier}</b>
            {technical && settings.hw.gpu_name && (
              <span>
                {" "}
                · GPU: {settings.hw.gpu_name} ({Math.round((settings.hw.vram_mb ?? 0) / 1024)} GB
                VRAM)
              </span>
            )}
            {technical && !settings.hw.gpu_name && <span> · no NVIDIA GPU</span>}
          </div>
        )}

        {/* Appearance */}
        <section className="flex items-center justify-between gap-3">
          <SectionLabel>Appearance</SectionLabel>
          <Segmented
            value={theme}
            onChange={(v) => setTheme(v as "system" | "light" | "dark")}
            options={[
              ["system", "System"],
              ["light", "Light"],
              ["dark", "Dark"],
            ]}
          />
        </section>

        {/* Microphone */}
        <section className="flex items-center justify-between gap-3">
          <SectionLabel>Microphone</SectionLabel>
          <Select
            style={{ maxWidth: "20rem" }}
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
        </section>

        {/* Hardware tier (transcription) */}
        <section className="space-y-3">
          {technical ? (
            <>
              <SectionLabel>Hardware tier</SectionLabel>
              <div className="flex flex-col">
                {TIERS.map((t) => (
                  <Checkbox
                    key={t.id}
                    type="radio"
                    name="tier"
                    checked={tier === t.id}
                    onChange={() => setTier(t.id)}
                    label={t.label}
                    description={t.desc}
                    style={{ padding: "6px 2px" }}
                  />
                ))}
              </div>
            </>
          ) : (
            <div className="flex items-center justify-between gap-3">
              <SectionLabel>Hardware tier</SectionLabel>
              <Segmented
                value={tier}
                onChange={setTier}
                options={TIERS.map(
                  (t) => [t.id, t.id === "cloud" ? "Cloud" : t.label, t.desc] as SegOption,
                )}
              />
            </div>
          )}

          {/* Cloud tier / Groq fallback → collect the Groq key here */}
          {showGroqCard && (
            <div ref={groqCardRef}>
              <Card tone="muted" pad="sm" radius="md">
                <div className="mb-2 flex items-center gap-2">
                  <span className="text-text" style={{ fontSize: "12.5px", fontWeight: 600 }}>
                    Groq
                  </span>
                  <Badge tone="warning" size="sm">
                    cloud
                  </Badge>
                  <Badge tone={groqHasKey ? "success" : "warning"} size="sm">
                    {groqHasKey ? "key ✓" : "needs key"}
                  </Badge>
                </div>
                <Input
                  type="password"
                  style={MONO_KEY}
                  placeholder={
                    settings?.has_groq_key ? "Groq API key saved — enter to replace" : "gsk_…"
                  }
                  value={groqKey}
                  onChange={(e) => setGroqKey(e.target.value)}
                />
                <p
                  className="text-text-3"
                  style={{ margin: "8px 0 0", fontSize: 11, lineHeight: 1.5 }}
                >
                  Cloud transcription uploads meeting audio to Groq — it{" "}
                  <span style={LEAVES_PILL}>LEAVES</span> this machine. Who-said-what (diarization)
                  still runs locally. Audio goes to Groq first, paced to stay inside the free
                  tier&apos;s hourly limit; whenever the quota is busy, this machine&apos;s GPU or
                  CPU transcribes in the meantime. Finished parts are saved as they complete, so a
                  quit or restart picks up where it left off — nothing is transcribed twice.
                  Applied when you press Save.
                </p>
              </Card>
            </div>
          )}

          {tier !== "cloud" && (
            <>
              <Checkbox
                checked={useGpu}
                onChange={(e) => setUseGpu(e.target.checked)}
                label="Use GPU for transcription when it is faster on this machine"
              />
              {useGpu && settings?.gpu_bench && (
                <p className="pl-7 text-text-3" style={{ fontSize: 11, lineHeight: 1.5 }}>
                  {settings.gpu_bench.verdict === "gpu"
                    ? `Speed test: GPU ${settings.gpu_bench.gpu_secs?.toFixed(0)} s vs CPU ${settings.gpu_bench.cpu_secs?.toFixed(0)} s — transcribing on the GPU.`
                    : settings.gpu_bench.gpu_secs != null && settings.gpu_bench.cpu_secs != null
                      ? `Speed test: GPU ${settings.gpu_bench.gpu_secs.toFixed(0)} s vs CPU ${settings.gpu_bench.cpu_secs.toFixed(0)} s — CPU is faster here, so it stays on CPU.`
                      : "GPU transcription failed on this machine — staying on CPU. Toggle off and on to test again."}
                </p>
              )}
              {useGpu && !settings?.gpu_bench && (
                <p className="pl-7 text-text-3" style={{ fontSize: 11, lineHeight: 1.5 }}>
                  A one-time speed test runs at the start of the next transcription.
                </p>
              )}
            </>
          )}

          <Checkbox
            checked={autoTranscribe}
            onChange={(e) => setAutoTranscribe(e.target.checked)}
            label="Transcribe automatically when a recording stops"
          />

          {technical && (
            <Checkbox
              checked={useGroq}
              onChange={(e) => setUseGroq(e.target.checked)}
              label="Use Groq for transcription (cloud first, local fills in)"
              description="Uploads meeting audio to Groq while the free tier's hourly quota lasts; when it's busy, your GPU or CPU transcribes instead of waiting. Finished parts are checkpointed — a restart never re-transcribes them. Diarization stays local. Bring your own key."
            />
          )}
        </section>

        {/* Transcription model (Technical) */}
        {technical && (
          <section className="space-y-3">
            <SectionLabel>Transcription model</SectionLabel>
            <p className="text-text-3" style={{ fontSize: 11, lineHeight: 1.5 }}>
              A specific choice here overrides the Hardware tier and “Maximum quality” settings.
              “Auto (per tier)” uses the tier’s default.
            </p>
            <Select
              style={{ maxWidth: "20rem" }}
              value={modelId}
              onChange={(e) => setModelId(e.target.value)}
            >
              <option value="">Auto (per tier)</option>
              {asrModels.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.display}
                </option>
              ))}
            </Select>
            <Checkbox
              checked={maxQuality}
              onChange={(e) => setMaxQuality(e.target.checked)}
              label="Maximum quality (full large-v3 — slower, ~1 GB download)"
            />
          </section>
        )}

        {/* Models on disk (Technical) */}
        {technical && (
          <section className="space-y-2">
            <SectionLabel>Models</SectionLabel>
            <div>
              {/* Transcription engine — the whisper.cpp binary that actually
                  runs the weights below. Weights alone can't transcribe, so its
                  state is surfaced explicitly rather than failing at run time. */}
              {settings && (
                <div
                  ref={engineRowRef}
                  className="flex items-center justify-between gap-3 border-t border-line-2 py-2"
                >
                  <div className="min-w-0">
                    <div
                      className="truncate text-text"
                      style={{ fontFamily: "var(--font-mono)", fontSize: "12.5px" }}
                    >
                      Transcription engine (whisper.cpp)
                    </div>
                    <div className="text-text-3" style={{ fontSize: 11, lineHeight: 1.5 }}>
                      {settings.engine_installed
                        ? "Installed — runs the models above on this machine."
                        : settings.engine_managed
                          ? "Required to transcribe. Downloaded weights can't run without it."
                          : "Required to transcribe. Install whisper.cpp yourself — macOS: brew install whisper-cpp; Linux: your package manager or build from source."}
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
                          <span
                            className="text-text-3"
                            style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}
                          >
                            {enginePct}%
                          </span>
                        </span>
                      ) : (
                        <span
                          className="text-text-3"
                          style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}
                        >
                          downloading…
                        </span>
                      )
                    ) : settings.engine_managed ? (
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => void download(WHISPER_ENGINE_ID)}
                      >
                        Install
                      </Button>
                    ) : (
                      <Badge tone="warning">missing</Badge>
                    )}
                  </span>
                </div>
              )}
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
                  <div
                    key={m.id}
                    className="flex items-center justify-between gap-3 border-t border-line-2 py-2"
                  >
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
                            <span
                              className="text-text-3"
                              style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}
                            >
                              {pct}%
                            </span>
                          </span>
                        ) : (
                          <span
                            className="text-text-3"
                            style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}
                          >
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
          </section>
        )}

        {/* AI provider */}
        <section className="space-y-2">
          <SectionLabel>AI provider (for Enhance &amp; Ask)</SectionLabel>
          <p className="text-text-3" style={{ fontSize: 11, lineHeight: 1.5 }}>
            Ollama runs entirely on this machine. OpenAI, Anthropic, and NVIDIA NIM send your notes
            + transcript to their APIs when you use Enhance or Ask.
          </p>
          {llm && llm.providers.length > 0 && (
            <Segmented
              full
              value={llmProvider}
              onChange={pickProvider}
              options={llm.providers.map((p) => [p.id, p.id] as SegOption)}
            />
          )}
          {(() => {
            const p = llm?.providers.find((x) => x.id === llmProvider);
            if (!p) return null;
            const hasKey = p.has_key || !!llmKey;
            return (
              <Card tone="muted" pad="sm" radius="md">
                <div
                  className="flex items-center gap-2"
                  style={{ marginBottom: p.is_local ? 0 : 8 }}
                >
                  <span className="text-text" style={{ fontSize: "12.5px", fontWeight: 600 }}>
                    {p.id}
                  </span>
                  <Badge tone={p.is_local ? "success" : "warning"} size="sm">
                    {p.is_local ? "local" : "cloud"}
                  </Badge>
                  {!p.is_local && (
                    <Badge tone={hasKey ? "success" : "warning"} size="sm">
                      {hasKey ? "key ✓" : "needs key"}
                    </Badge>
                  )}
                </div>
                {p.is_local ? (
                  (() => {
                    // Managed Ollama block: install → auto-managed server → model.
                    const effModel = (llmModel || p.default_model || "llama3.1").trim();
                    const modelReady = !!ollama?.models.some(
                      (m) => m.name === effModel || m.name.split(":")[0] === effModel.split(":")[0],
                    );
                    // Progress for the runtime install or the model pull.
                    const prog =
                      modelProgress &&
                      (modelProgress.id === "ollama-bin" ||
                        modelProgress.id === `ollama:${effModel}`) &&
                      ollamaBusy !== null &&
                      modelProgress.total > 0
                        ? modelProgress
                        : null;
                    const pct = prog ? Math.round((prog.downloaded / prog.total) * 100) : null;
                    return (
                      <div className="space-y-2">
                        <div className="flex flex-wrap items-center gap-1.5">
                          {ollama ? (
                            <>
                              <Badge tone={ollama.running ? "success" : "neutral"} size="sm">
                                {ollama.running
                                  ? ollama.managed
                                    ? "server running (managed by app)"
                                    : "server running"
                                  : ollama.installed
                                    ? "server off — starts automatically"
                                    : "not installed"}
                              </Badge>
                              {ollama.running && (
                                <Badge tone={modelReady ? "success" : "warning"} size="sm">
                                  {modelReady ? `${effModel} ready` : `${effModel} not downloaded`}
                                </Badge>
                              )}
                            </>
                          ) : (
                            <span className="text-text-3" style={{ fontSize: 12 }}>
                              checking Ollama…
                            </span>
                          )}
                        </div>
                        <span className="block text-text-2" style={{ fontSize: 12 }}>
                          Runs entirely on this machine — no API key needed. The app starts and
                          stops the server for you and keeps its models in the app data folder.
                        </span>
                        <div className="flex flex-wrap items-center gap-2">
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
                          {ollama && !ollama.installed && !ollama.can_install && (
                            <span className="text-text-2" style={{ fontSize: 12 }}>
                              Install Ollama from ollama.com, then Test connection.
                            </span>
                          )}
                          {ollama && ollama.installed && !modelReady && (
                            <Button
                              variant="primary"
                              size="sm"
                              disabled={ollamaBusy !== null}
                              onClick={() => void pullOllamaModel(effModel)}
                            >
                              {ollamaBusy === "pull"
                                ? "Downloading…"
                                : `Download ${effModel} model`}
                            </Button>
                          )}
                          {ollamaMsg && (
                            <span className="text-text-2" style={{ fontSize: 12 }}>
                              {ollamaMsg}
                            </span>
                          )}
                        </div>
                        {pct !== null && (
                          <div className="flex items-center gap-2">
                            <ProgressBar value={pct} style={{ width: 180 }} />
                            <span className="text-text-3" style={{ fontSize: 11 }}>
                              {pct}%
                            </span>
                          </div>
                        )}
                        {swapOffer && !modelReady && (
                          <div className="flex flex-wrap items-center gap-2">
                            <span className="text-text-2" style={{ fontSize: 12 }}>
                              “{swapOffer.to}” isn’t downloaded yet — Enhance and Ask would fail
                              until it is.
                            </span>
                            <Button
                              variant="primary"
                              size="sm"
                              disabled={ollamaBusy !== null}
                              onClick={() => void pullOllamaModel(swapOffer.to)}
                            >
                              {ollamaBusy === "pull" ? "Downloading…" : `Download ${swapOffer.to}`}
                            </Button>
                            <Button variant="ghost" size="sm" onClick={() => setSwapOffer(null)}>
                              Later
                            </Button>
                          </div>
                        )}
                        {deleteOffer && (
                          <div className="flex flex-wrap items-center gap-2">
                            <span className="text-text-2" style={{ fontSize: 12 }}>
                              Switched models — delete “{deleteOffer.name}” to free{" "}
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
                            <Button variant="ghost" size="sm" onClick={() => setDeleteOffer(null)}>
                              Keep
                            </Button>
                          </div>
                        )}
                        {ollama && ollama.running && ollama.models.length > 0 && (
                          <div className="space-y-1">
                            <span className="block text-text-3" style={{ fontSize: 11 }}>
                              Models on disk
                            </span>
                            {ollama.models.map((m) => {
                              const inUse =
                                m.name === effModel ||
                                m.name.split(":")[0] === effModel.split(":")[0];
                              return (
                                <div key={m.name} className="flex items-center gap-2">
                                  <span className="text-text-2" style={{ fontSize: 12 }}>
                                    {m.name}
                                  </span>
                                  <span className="text-text-3" style={{ fontSize: 11 }}>
                                    {gb(m.size)}
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
                      </div>
                    );
                  })()
                ) : (
                  <Input
                    type="password"
                    style={MONO_KEY}
                    placeholder={
                      p.has_key ? "API key saved — enter to replace" : `Paste your ${p.id} API key`
                    }
                    value={llmKey}
                    onChange={(e) => setLlmKey(e.target.value)}
                  />
                )}
                {technical && (
                  <div className="mt-2 grid grid-cols-2 gap-2">
                    <Input
                      placeholder={`model (default: ${p.default_model ?? ""})`}
                      value={llmModel}
                      onChange={(e) => setLlmModel(e.target.value)}
                    />
                    <Input
                      placeholder={
                        llmProvider === "ollama"
                          ? "base URL (default: localhost:11434)"
                          : "base URL (optional)"
                      }
                      value={llmBaseUrl}
                      onChange={(e) => setLlmBaseUrl(e.target.value)}
                    />
                  </div>
                )}
                <div className="mt-3 flex items-center gap-2">
                  <Button
                    variant="primary"
                    size="sm"
                    onClick={() => void saveLlm()}
                    disabled={llmBusy}
                  >
                    {hasKey && !p.is_local ? "Update" : "Save provider"}
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => void testLlm()}
                    disabled={llmBusy}
                  >
                    Test connection
                  </Button>
                  {llmTest && (
                    <span className="text-text-2" style={{ fontSize: 12 }}>
                      {llmTest}
                    </span>
                  )}
                </div>
              </Card>
            );
          })()}
        </section>

        {/* Calendars */}
        <section className="space-y-2">
          <SectionLabel>Calendars</SectionLabel>
          <p className="text-text-3" style={{ fontSize: 11, lineHeight: 1.5 }}>
            Connect in one click with the built-in app — or bring your own OAuth app (Technical): a
            Google Cloud "Desktop app" client (ID + secret) and/or an Azure app registration with a
            public client + loopback redirect. Tokens are stored in the Windows keychain. See the
            README for a step-by-step.
          </p>
          <Card tone="surface" pad="md">
            <div className="flex items-center justify-between gap-2">
              <span className="text-text" style={{ fontSize: 13 }}>
                Google Calendar
              </span>
              {cal?.google_connected ? (
                <span className="flex items-center gap-2">
                  <Badge tone="success" dot>
                    connected
                  </Badge>
                  <Button
                    variant="outline"
                    size="sm"
                    className="hover:text-rec"
                    disabled={calBusy !== null}
                    onClick={() => void disconnectCal("google")}
                  >
                    disconnect
                  </Button>
                </span>
              ) : (
                <Button
                  variant="primary"
                  size="sm"
                  disabled={calBusy !== null || !cal?.google_configured}
                  onClick={() => void connectCal("google")}
                >
                  {calBusy === "google" ? "waiting for browser…" : "Connect"}
                </Button>
              )}
            </div>
            {technical && (
              <div className="mt-2.5 grid grid-cols-2 gap-2">
                <Input
                  style={MONO_KEY}
                  placeholder="Client ID (….apps.googleusercontent.com)"
                  value={googleId}
                  onChange={(e) => setGoogleId(e.target.value)}
                />
                <Input
                  type="password"
                  style={MONO_KEY}
                  placeholder={
                    cal?.google_has_secret
                      ? "Client secret saved — enter to replace"
                      : "Client secret"
                  }
                  value={googleSecret}
                  onChange={(e) => setGoogleSecret(e.target.value)}
                />
              </div>
            )}
          </Card>
          <Card tone="surface" pad="md">
            <div className="flex items-center justify-between gap-2">
              <span className="text-text" style={{ fontSize: 13 }}>
                Microsoft 365 / Outlook
              </span>
              {cal?.ms_connected ? (
                <span className="flex items-center gap-2">
                  <Badge tone="success" dot>
                    connected
                  </Badge>
                  <Button
                    variant="outline"
                    size="sm"
                    className="hover:text-rec"
                    disabled={calBusy !== null}
                    onClick={() => void disconnectCal("msgraph")}
                  >
                    disconnect
                  </Button>
                </span>
              ) : (
                <Button
                  variant="primary"
                  size="sm"
                  disabled={calBusy !== null || !cal?.ms_configured}
                  onClick={() => void connectCal("msgraph")}
                >
                  {calBusy === "msgraph" ? "waiting for browser…" : "Connect"}
                </Button>
              )}
            </div>
            {technical && (
              <div className="mt-2.5">
                <Input
                  style={MONO_KEY}
                  placeholder="Application (client) ID — no secret needed"
                  value={msId}
                  onChange={(e) => setMsId(e.target.value)}
                />
              </div>
            )}
          </Card>
          {calList.length > 0 && (
            <div className="space-y-1.5 pt-1">
              <SectionLabel style={{ display: "block" }}>Calendars in “Up next”</SectionLabel>
              <p className="text-text-3" style={{ fontSize: 11, lineHeight: 1.5 }}>
                Meetings from the calendars you enable here feed the sidebar. Only events with a
                join link are shown.
              </p>
              {calList.map((c) => (
                <div
                  key={`${c.provider}-${c.id}`}
                  className="flex items-center justify-between gap-2"
                >
                  <Checkbox
                    checked={c.enabled}
                    onChange={(e) => void toggleCalendar(c, e.target.checked)}
                    label={c.name}
                  />
                  <span className="flex flex-none items-center gap-1.5">
                    {c.primary && (
                      <Badge tone="neutral" size="sm">
                        primary
                      </Badge>
                    )}
                    <Badge tone="neutral" size="sm">
                      {c.provider === "google" ? "Google" : "Outlook"}
                    </Badge>
                  </span>
                </div>
              ))}
            </div>
          )}
          {technical && (
            <div className="flex items-center gap-2">
              <Button variant="primary" size="sm" onClick={() => void saveCal()}>
                Save calendar config
              </Button>
              {calMsg && (
                <span className="text-text-2" style={{ fontSize: 12 }}>
                  {calMsg}
                </span>
              )}
            </div>
          )}
        </section>

        {/* Note templates */}
        <section className="space-y-2">
          <div className="flex items-center justify-between">
            <SectionLabel>Note templates</SectionLabel>
            <Button
              variant="outline"
              size="sm"
              startIcon={<Plus size={14} strokeWidth={1.75} />}
              onClick={() =>
                setEditingTpl({
                  id: "",
                  name: "New template",
                  system_prompt: "You are an expert meeting-notes editor. Never invent facts.",
                  structure_hint: "## Summary\n## Action items",
                  built_in: false,
                })
              }
            >
              New
            </Button>
          </div>
          {editingTpl ? (
            <Card tone="surface" pad="md">
              <div className="space-y-2">
                <Input
                  value={editingTpl.name}
                  onChange={(e) => setEditingTpl({ ...editingTpl, name: e.target.value })}
                  placeholder="Template name"
                />
                <textarea
                  value={editingTpl.system_prompt}
                  onChange={(e) => setEditingTpl({ ...editingTpl, system_prompt: e.target.value })}
                  rows={3}
                  style={TEXTAREA}
                  placeholder="System prompt"
                />
                <textarea
                  value={editingTpl.structure_hint}
                  onChange={(e) => setEditingTpl({ ...editingTpl, structure_hint: e.target.value })}
                  rows={3}
                  style={{ ...TEXTAREA, fontFamily: "var(--font-mono)" }}
                  placeholder="Structure hint (markdown headings)"
                />
                <div className="flex gap-2">
                  <Button variant="primary" size="sm" onClick={() => void saveTpl()}>
                    Save template
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => setEditingTpl(null)}>
                    Cancel
                  </Button>
                </div>
              </div>
            </Card>
          ) : (
            <div>
              {templates.map((t) => (
                <div
                  key={t.id}
                  className="flex items-center justify-between rounded-lg px-2 py-1.5 hover:bg-surface-3"
                  style={{ fontSize: "13px" }}
                >
                  <span className="text-text">{t.name}</span>
                  <span className="flex items-center gap-2">
                    {t.built_in && (
                      <Badge tone="neutral" size="sm">
                        built-in
                      </Badge>
                    )}
                    <Button variant="ghost" size="sm" onClick={() => setEditingTpl(t)}>
                      edit
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      className="hover:text-rec"
                      onClick={() => void removeTpl(t.id)}
                    >
                      delete
                    </Button>
                  </span>
                </div>
              ))}
            </div>
          )}
        </section>

        {/* MCP */}
        <section className="space-y-2">
          {/* SectionLabel's inline margin:0 overrides the space-y-2 gap
              (Tailwind v4 puts it on the label as margin-bottom), leaving the
              label flush against the dark card — restore the 8px here */}
          <SectionLabel style={{ display: "block", marginBottom: 8 }}>
            Chat with your notes (MCP)
          </SectionLabel>
          <Card
            pad="lg"
            style={{
              background: "var(--brand-ink)",
              color: "#fff",
              border: "1px solid transparent",
            }}
          >
            <p
              style={{
                margin: "0 0 12px",
                fontSize: 13,
                lineHeight: 1.5,
                color: "rgba(255,255,255,.72)",
              }}
            >
              Fly on the Wall ships a local MCP server. Add this to your MCP client (Claude
              Desktop's{" "}
              <code
                style={{
                  borderRadius: 4,
                  background: "rgba(255,255,255,.1)",
                  padding: "1px 4px",
                  fontFamily: "var(--font-mono)",
                  fontSize: 11,
                }}
              >
                claude_desktop_config.json
              </code>
              , or any client) to search and read your notes and transcripts — everything stays on
              this machine.
            </p>
            {mcpJson && (
              <div style={{ position: "relative" }}>
                <pre
                  style={{
                    margin: 0,
                    overflowX: "auto",
                    whiteSpace: "pre",
                    borderRadius: 10,
                    border: "1px solid rgba(255,255,255,.15)",
                    background: "rgba(255,255,255,.08)",
                    padding: 12,
                    fontFamily: "var(--font-mono)",
                    fontSize: 12,
                    color: "#fff",
                  }}
                >
                  {mcpJson}
                </pre>
                <span style={{ position: "absolute", right: 8, top: 8 }}>
                  <Button
                    variant="primary"
                    size="sm"
                    startIcon={
                      mcpCopied ? (
                        <Check size={14} strokeWidth={1.75} />
                      ) : (
                        <Copy size={14} strokeWidth={1.75} />
                      )
                    }
                    onClick={() => {
                      void navigator.clipboard.writeText(mcpJson).then(() => {
                        setMcpCopied(true);
                        window.setTimeout(() => setMcpCopied(false), 1500);
                      });
                    }}
                  >
                    {mcpCopied ? "Copied" : "Copy"}
                  </Button>
                </span>
              </div>
            )}
            {/* On-demand backfill: meetings transcribed before item
                extraction existed get their decisions/action items/figures
                extracted with the selected AI provider. New meetings are
                extracted automatically after transcription. */}
            <div className="flex items-center gap-3" style={{ marginTop: 12 }}>
              <Button
                variant="outline"
                size="sm"
                disabled={backfillBusy}
                style={{ color: "#fff", borderColor: "rgba(255,255,255,.3)" }}
                onClick={() => void runBackfill()}
              >
                {backfillBusy ? "Extracting…" : "Extract items from past meetings"}
              </Button>
              {backfillMsg && (
                <span style={{ fontSize: 11.5, color: "rgba(255,255,255,.72)" }}>
                  {backfillMsg}
                </span>
              )}
            </div>
          </Card>
        </section>

        {/* Diagnostics (Technical) — where the rolling log files live */}
        {technical && (
          <section className="flex items-center justify-between gap-3">
            <div>
              <SectionLabel>Diagnostics</SectionLabel>
              <p className="text-text-3" style={{ margin: 0, fontSize: 11, lineHeight: 1.5 }}>
                The app writes local log files (last few days) for troubleshooting. Nothing leaves
                this machine.
              </p>
            </div>
            <Button variant="outline" size="sm" onClick={() => void api.revealLogsDir()}>
              Open logs folder
            </Button>
          </section>
        )}

        {/* App updates */}
        <section className="space-y-2">
          {updater.supported ? (
            <Card tone="surface" pad="md">
              <div className="space-y-2">
                <div className="flex items-center justify-between gap-3">
                  <span className="text-text-2" style={{ fontSize: 13 }}>
                    Current version{" "}
                    <span className="font-semibold text-text">v{appVersion ?? "…"}</span>
                  </span>
                  {(updater.phase === "idle" ||
                    updater.phase === "upToDate" ||
                    updater.phase === "error") && (
                    <Button variant="outline" size="sm" onClick={updater.check}>
                      Check for updates
                    </Button>
                  )}
                  {updater.phase === "checking" && (
                    <Button variant="outline" size="sm" disabled>
                      Checking…
                    </Button>
                  )}
                  {updater.phase === "available" && (
                    <Button
                      variant="primary"
                      size="sm"
                      disabled={recordingActive}
                      onClick={updater.downloadAndInstall}
                    >
                      Download &amp; install v{updater.version}
                    </Button>
                  )}
                  {updater.phase === "downloading" && (
                    <span className="flex items-center gap-2">
                      <ProgressBar
                        value={updater.progress == null ? null : Math.round(updater.progress * 100)}
                        style={{ width: 96 }}
                      />
                      <span
                        className="text-text-3"
                        style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}
                      >
                        {updater.progress == null
                          ? "downloading…"
                          : `${Math.round(updater.progress * 100)}%`}
                      </span>
                    </span>
                  )}
                  {updater.phase === "ready" && (
                    <Button
                      variant="primary"
                      size="sm"
                      disabled={recordingActive}
                      onClick={updater.restart}
                    >
                      Restart now
                    </Button>
                  )}
                  {updater.phase === "installing" && (
                    <span className="text-text-2" style={{ fontSize: 12 }}>
                      Installing — Fly on the Wall will restart…
                    </span>
                  )}
                </div>
                {updater.phase === "upToDate" && (
                  <p className="text-success-text" style={{ fontSize: 12, fontWeight: 500 }}>
                    You're on the latest version ✓
                  </p>
                )}
                {updater.phase === "error" && (
                  <p className="text-error-text" style={{ fontSize: 12 }}>
                    ✗ {updater.error}
                  </p>
                )}
                {updater.phase === "ready" && !recordingActive && (
                  <p className="text-text-2" style={{ fontSize: 12 }}>
                    Update downloaded — restart when you're ready.
                  </p>
                )}
                {recordingActive &&
                  (updater.phase === "available" || updater.phase === "ready") && (
                    <p className="text-text-2" style={{ fontSize: 12 }}>
                      Recording in progress — the update waits until you're done.
                    </p>
                  )}
              </div>
            </Card>
          ) : (
            <p className="text-text-3" style={{ fontSize: 11, lineHeight: 1.5 }}>
              Auto-update is Windows-only for now — new versions are published on the GitHub
              releases page.
            </p>
          )}
        </section>
      </div>
    </Modal>
  );
}
