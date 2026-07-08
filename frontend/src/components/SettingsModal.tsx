import { useEffect, useState } from "react";
import { api } from "../api";
import type {
  AsrSettings,
  AudioDevice,
  CalendarStatus,
  LlmSettings,
  ModelProgress,
  Template,
} from "../types";
import { Btn, ModalShell, SectionLabel } from "./ui";
import type { Updater } from "../updater";

interface Props {
  modelProgress: ModelProgress | null;
  updater: Updater;
  recordingActive: boolean;
  appVersion: string | null;
  onClose: () => void;
}

const TIERS = [
  { id: "light", label: "Light", desc: "Whisper small (Q5) — ~2 GB RAM, fine on old laptops" },
  { id: "balanced", label: "Balanced", desc: "large-v3-turbo (Q5) — near-large accuracy on CPU" },
  { id: "best", label: "Best", desc: "large-v3-turbo (Q5), large-v3 with max-quality toggle" },
  { id: "cloud", label: "Cloud (Groq)", desc: "For weak devices — audio LEAVES this machine" },
];

/* Shared field chrome for inputs/selects/textareas in this modal. */
const FIELD =
  "rounded-lg border border-line bg-surface px-3 py-1.5 text-sm text-ink outline-none placeholder:text-mute focus:border-coral";
const PROMPT_BOX =
  "w-full rounded-lg border border-dashed border-line bg-peach-2 p-2 text-[13px] text-ink outline-none placeholder:text-mute focus:border-coral";

function gb(bytes: number): string {
  return bytes >= 1_000_000_000
    ? `${(bytes / 1_000_000_000).toFixed(1)} GB`
    : `${Math.round(bytes / 1_000_000)} MB`;
}

export default function SettingsModal({
  modelProgress,
  updater,
  recordingActive,
  appVersion,
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

  // LLM provider state
  const [llm, setLlm] = useState<LlmSettings | null>(null);
  const [llmProvider, setLlmProvider] = useState("ollama");
  const [llmModel, setLlmModel] = useState("");
  const [llmBaseUrl, setLlmBaseUrl] = useState("");
  const [llmKey, setLlmKey] = useState("");
  const [llmTest, setLlmTest] = useState<string | null>(null);
  const [llmBusy, setLlmBusy] = useState(false);

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

  const loadCal = () =>
    api.getCalendarSettings().then((c) => {
      setCal(c);
      setGoogleId(c.google_client_id);
      setMsId(c.ms_client_id);
    });

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

  useEffect(() => {
    load().catch(console.error);
    loadLlm().catch(console.error);
    api.listTemplates().then(setTemplates).catch(console.error);
    loadCal().catch(console.error);
    api.mcpConfig().then(setMcpJson).catch(console.error);
    api.listMicDevices().then(setMics).catch(console.error);
    api
      .getAppSetting("capture.mic_device_id")
      .then((v) => setMicId(v ?? ""))
      .catch(console.error);
  }, []);

  const pickProvider = (id: string) => {
    setLlmProvider(id);
    const info = llm?.providers.find((p) => p.id === id);
    setLlmModel(info?.model ?? "");
    setLlmBaseUrl(info?.base_url ?? "");
    setLlmKey("");
    setLlmTest(null);
  };

  const saveLlm = async () => {
    setLlmBusy(true);
    setLlmTest(null);
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

  const download = async (id: string) => {
    setDownloading(id);
    try {
      await api.downloadModel(id);
      await load();
    } catch (e) {
      console.error(e);
    } finally {
      setDownloading(null);
    }
  };

  const asrModels = settings?.models.filter((m) => m.id.startsWith("ggml-")) ?? [];
  const otherModels = settings?.models.filter((m) => !m.id.startsWith("ggml-")) ?? [];

  return (
    /* Wrapper catches bubbled overlay clicks so click-outside still closes;
       the card content stops propagation. */
    <div onClick={onClose}>
      <ModalShell className="w-[640px] max-w-[92vw] p-0">
        <div onClick={(e) => e.stopPropagation()}>
          <div className="flex items-center justify-between border-b border-line px-6 pt-5 pb-4">
            <h2 className="font-display text-xl font-bold tracking-tight text-ink">Settings</h2>
            <Btn variant="ghost" size="sm" onClick={onClose} aria-label="Close settings">
              ✕
            </Btn>
          </div>

          <div className="max-h-[70vh] space-y-7 overflow-y-auto px-6 py-5">
            {settings && (
              <div className="rounded-[12px] bg-cream px-3.5 py-2.5 text-xs text-ink-2">
                This machine: {settings.hw.cpu_cores} cores · {settings.hw.ram_gb} GB RAM
                {settings.hw.gpu_name
                  ? ` · ${settings.hw.gpu_name} (${Math.round((settings.hw.vram_mb ?? 0) / 1024)} GB VRAM)`
                  : " · no NVIDIA GPU"}
                {" — recommended tier: "}
                <span className="font-semibold capitalize text-ink">
                  {settings.hw.recommended_tier}
                </span>
              </div>
            )}

            <section className="space-y-2">
              <SectionLabel>Microphone</SectionLabel>
              <select
                value={micId}
                onChange={(e) => {
                  setMicId(e.target.value);
                  void api.setAppSetting("capture.mic_device_id", e.target.value);
                }}
                className={`${FIELD} w-full max-w-80`}
              >
                <option value="">System default</option>
                {mics.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.name}
                    {m.is_default ? " (default)" : ""}
                  </option>
                ))}
              </select>
            </section>

            <section className="space-y-2">
              <SectionLabel>Hardware tier</SectionLabel>
              <div className="space-y-0.5">
                {TIERS.map((t) => (
                  <label
                    key={t.id}
                    className="flex cursor-pointer items-start gap-2.5 rounded-lg px-2 py-1.5 hover:bg-peach-2"
                  >
                    <input
                      type="radio"
                      checked={tier === t.id}
                      onChange={() => setTier(t.id)}
                      className="mt-1 accent-coral"
                    />
                    <span className="text-sm">
                      <span className="font-medium text-ink">{t.label}</span>
                      <span className="ml-2 text-xs text-mute">{t.desc}</span>
                    </span>
                  </label>
                ))}
              </div>

              <label className="flex items-center gap-2 text-sm text-ink-2">
                <input
                  type="checkbox"
                  className="accent-coral"
                  checked={useGpu}
                  onChange={(e) => setUseGpu(e.target.checked)}
                />
                Use GPU for transcription when it is faster on this machine
              </label>
              {useGpu && settings?.gpu_bench && (
                <p className="pl-6 text-xs text-mute">
                  {settings.gpu_bench.verdict === "gpu"
                    ? `Speed test: GPU ${settings.gpu_bench.gpu_secs?.toFixed(0)} s vs CPU ${settings.gpu_bench.cpu_secs?.toFixed(0)} s — transcribing on the GPU.`
                    : settings.gpu_bench.gpu_secs != null && settings.gpu_bench.cpu_secs != null
                      ? `Speed test: GPU ${settings.gpu_bench.gpu_secs.toFixed(0)} s vs CPU ${settings.gpu_bench.cpu_secs.toFixed(0)} s — CPU is faster here, so it stays on CPU.`
                      : "GPU transcription failed on this machine — staying on CPU. Toggle off and on to test again."}
                </p>
              )}
              {useGpu && !settings?.gpu_bench && (
                <p className="pl-6 text-xs text-mute">
                  A one-time speed test runs at the start of the next transcription.
                </p>
              )}
            </section>

            <section className="space-y-2">
              <SectionLabel>Model</SectionLabel>
              <select
                value={modelId}
                onChange={(e) => setModelId(e.target.value)}
                className={`${FIELD} w-full max-w-80`}
              >
                <option value="">Auto (per tier)</option>
                {asrModels.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.display}
                  </option>
                ))}
              </select>

              <label className="flex items-center gap-2 text-sm text-ink-2">
                <input
                  type="checkbox"
                  className="accent-coral"
                  checked={maxQuality}
                  onChange={(e) => setMaxQuality(e.target.checked)}
                />
                Maximum quality (full large-v3 — slower, ~1 GB download)
              </label>
              <label className="flex items-center gap-2 text-sm text-ink-2">
                <input
                  type="checkbox"
                  className="accent-coral"
                  checked={autoTranscribe}
                  onChange={(e) => setAutoTranscribe(e.target.checked)}
                />
                Transcribe automatically when a recording stops
              </label>

              <div className="rounded-[12px] border border-line bg-peach-2 p-3 text-[13px] text-clay">
                <label className="flex items-center gap-2 font-semibold">
                  <input
                    type="checkbox"
                    className="accent-coral"
                    checked={useGroq}
                    onChange={(e) => setUseGroq(e.target.checked)}
                  />
                  Use Groq for transcription (cloud fallback)
                </label>
                <p className="mt-1 text-xs opacity-80">
                  When enabled, meeting audio is uploaded to Groq for transcription. Who-said-what
                  (diarization) still runs locally. Bring your own key — free tier available.
                </p>
                <input
                  type="password"
                  placeholder={
                    settings?.has_groq_key ? "Groq API key saved — enter to replace" : "gsk_…"
                  }
                  value={groqKey}
                  onChange={(e) => setGroqKey(e.target.value)}
                  className={`${FIELD} mt-2 w-full`}
                />
              </div>
            </section>

            <section className="space-y-2">
              <SectionLabel>Models &amp; engines on disk</SectionLabel>
              <div className="rounded-[14px] border border-line bg-surface px-4 py-1">
                {[...asrModels, ...otherModels].map((m) => (
                  <div
                    key={m.id}
                    className="flex items-center justify-between border-b border-line-2 py-2 text-xs last:border-b-0"
                  >
                    <span className="truncate text-ink-2">{m.display}</span>
                    <span className="ml-2 flex shrink-0 items-center gap-2">
                      <span className="font-mono text-[11px] text-mute">{gb(m.bytes)}</span>
                      {m.installed ? (
                        <span className="font-medium text-spk-teal">installed</span>
                      ) : downloading === m.id ? (
                        modelProgress && modelProgress.id === m.id && modelProgress.total > 0 ? (
                          <span className="flex items-center gap-2">
                            <span className="h-1.5 w-24 overflow-hidden rounded-full bg-line">
                              <span
                                className="block h-full rounded-full bg-coral"
                                style={{
                                  width: `${Math.round((modelProgress.downloaded / modelProgress.total) * 100)}%`,
                                }}
                              />
                            </span>
                            <span className="font-mono text-[11px] text-mute">
                              {Math.round((modelProgress.downloaded / modelProgress.total) * 100)}%
                            </span>
                          </span>
                        ) : (
                          <span className="font-mono text-[11px] text-mute">downloading…</span>
                        )
                      ) : (
                        <Btn variant="primary" size="sm" onClick={() => void download(m.id)}>
                          Download
                        </Btn>
                      )}
                    </span>
                  </div>
                ))}
              </div>
            </section>

            <section className="space-y-2">
              <SectionLabel>AI provider (for Enhance &amp; Ask)</SectionLabel>
              <div className="flex flex-wrap gap-2">
                {llm?.providers.map((p) => (
                  <button
                    key={p.id}
                    onClick={() => pickProvider(p.id)}
                    className={`inline-flex cursor-pointer items-center rounded-lg border px-2.5 py-1 text-xs font-semibold transition-colors ${
                      llmProvider === p.id
                        ? "border-coral bg-peach text-clay"
                        : "border-line bg-surface text-ink-2 hover:bg-peach-2"
                    }`}
                  >
                    {p.id}
                    <span
                      className={`ml-1.5 rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide ${
                        p.is_local
                          ? "bg-[color-mix(in_srgb,var(--color-spk-teal)_15%,transparent)] text-spk-teal"
                          : "bg-[color-mix(in_srgb,var(--color-spk-amber)_15%,transparent)] text-spk-amber"
                      }`}
                    >
                      {p.is_local ? "local" : "cloud"}
                    </span>
                    {p.has_key && <span className="ml-1.5 text-[10px] text-mute">key ✓</span>}
                  </button>
                ))}
              </div>
              <div className="grid grid-cols-2 gap-2">
                <input
                  placeholder={`model (default: ${
                    llm?.providers.find((p) => p.id === llmProvider)?.default_model ?? ""
                  })`}
                  value={llmModel}
                  onChange={(e) => setLlmModel(e.target.value)}
                  className={FIELD}
                />
                <input
                  placeholder={
                    llmProvider === "ollama"
                      ? "base URL (default: localhost:11434)"
                      : "base URL (optional)"
                  }
                  value={llmBaseUrl}
                  onChange={(e) => setLlmBaseUrl(e.target.value)}
                  className={FIELD}
                />
                {llmProvider !== "ollama" && (
                  <input
                    type="password"
                    placeholder={
                      llm?.providers.find((p) => p.id === llmProvider)?.has_key
                        ? "API key saved — enter to replace"
                        : "API key"
                    }
                    value={llmKey}
                    onChange={(e) => setLlmKey(e.target.value)}
                    className={`${FIELD} col-span-2`}
                  />
                )}
              </div>
              <div className="flex items-center gap-2">
                <Btn variant="primary" size="sm" onClick={() => void saveLlm()} disabled={llmBusy}>
                  Save provider
                </Btn>
                <Btn variant="outline" size="sm" onClick={() => void testLlm()} disabled={llmBusy}>
                  Test connection
                </Btn>
                {llmTest && <span className="text-xs text-ink-2">{llmTest}</span>}
              </div>
              <p className="text-[11px] text-mute">
                Ollama runs entirely on this machine. OpenAI, Anthropic, and NVIDIA NIM send your
                notes + transcript to their APIs when you use Enhance or Ask.
              </p>
            </section>

            <section className="space-y-2">
              <SectionLabel>Calendars</SectionLabel>
              <p className="text-[11px] text-mute">
                Bring your own OAuth app: a Google Cloud "Desktop app" client (ID + secret) and/or
                an Azure app registration with a public client + loopback redirect. Tokens are
                stored in the Windows keychain. See the README for a step-by-step.
              </p>
              <div className="space-y-2 rounded-[14px] border border-line bg-surface p-4">
                <div className="flex items-center justify-between">
                  <span className="text-[13px] font-semibold text-ink">Google Calendar</span>
                  {cal?.google_connected ? (
                    <span className="flex items-center gap-2">
                      <span className="text-xs font-medium text-spk-teal">connected</span>
                      <Btn
                        variant="outline"
                        size="sm"
                        className="hover:text-rec"
                        disabled={calBusy !== null}
                        onClick={() => void disconnectCal("google")}
                      >
                        disconnect
                      </Btn>
                    </span>
                  ) : (
                    <Btn
                      variant="primary"
                      size="sm"
                      disabled={calBusy !== null || !googleId}
                      onClick={() => void connectCal("google")}
                    >
                      {calBusy === "google" ? "waiting for browser…" : "Connect"}
                    </Btn>
                  )}
                </div>
                <input
                  placeholder="Client ID (….apps.googleusercontent.com)"
                  value={googleId}
                  onChange={(e) => setGoogleId(e.target.value)}
                  className={`${FIELD} w-full`}
                />
                <input
                  type="password"
                  placeholder={
                    cal?.google_has_secret
                      ? "Client secret saved — enter to replace"
                      : "Client secret"
                  }
                  value={googleSecret}
                  onChange={(e) => setGoogleSecret(e.target.value)}
                  className={`${FIELD} w-full`}
                />
              </div>
              <div className="space-y-2 rounded-[14px] border border-line bg-surface p-4">
                <div className="flex items-center justify-between">
                  <span className="text-[13px] font-semibold text-ink">
                    Microsoft 365 / Outlook
                  </span>
                  {cal?.ms_connected ? (
                    <span className="flex items-center gap-2">
                      <span className="text-xs font-medium text-spk-teal">connected</span>
                      <Btn
                        variant="outline"
                        size="sm"
                        className="hover:text-rec"
                        disabled={calBusy !== null}
                        onClick={() => void disconnectCal("msgraph")}
                      >
                        disconnect
                      </Btn>
                    </span>
                  ) : (
                    <Btn
                      variant="primary"
                      size="sm"
                      disabled={calBusy !== null || !msId}
                      onClick={() => void connectCal("msgraph")}
                    >
                      {calBusy === "msgraph" ? "waiting for browser…" : "Connect"}
                    </Btn>
                  )}
                </div>
                <input
                  placeholder="Application (client) ID — no secret needed"
                  value={msId}
                  onChange={(e) => setMsId(e.target.value)}
                  className={`${FIELD} w-full`}
                />
              </div>
              <div className="flex items-center gap-2">
                <Btn variant="primary" size="sm" onClick={() => void saveCal()}>
                  Save calendar config
                </Btn>
                {calMsg && <span className="text-xs text-ink-2">{calMsg}</span>}
              </div>
            </section>

            <section className="space-y-2">
              <div className="flex items-center justify-between">
                <SectionLabel>Note templates</SectionLabel>
                <Btn
                  variant="outline"
                  size="sm"
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
                  + New
                </Btn>
              </div>
              {editingTpl ? (
                <div className="space-y-2 rounded-[14px] border border-line bg-surface p-4">
                  <input
                    value={editingTpl.name}
                    onChange={(e) => setEditingTpl({ ...editingTpl, name: e.target.value })}
                    className={`${FIELD} w-full`}
                    placeholder="Template name"
                  />
                  <textarea
                    value={editingTpl.system_prompt}
                    onChange={(e) =>
                      setEditingTpl({ ...editingTpl, system_prompt: e.target.value })
                    }
                    rows={3}
                    className={PROMPT_BOX}
                    placeholder="System prompt"
                  />
                  <textarea
                    value={editingTpl.structure_hint}
                    onChange={(e) =>
                      setEditingTpl({ ...editingTpl, structure_hint: e.target.value })
                    }
                    rows={3}
                    className={`${PROMPT_BOX} font-mono`}
                    placeholder="Structure hint (markdown headings)"
                  />
                  <div className="flex gap-2">
                    <Btn variant="primary" size="sm" onClick={() => void saveTpl()}>
                      Save template
                    </Btn>
                    <Btn variant="outline" size="sm" onClick={() => setEditingTpl(null)}>
                      Cancel
                    </Btn>
                  </div>
                </div>
              ) : (
                <div>
                  {templates.map((t) => (
                    <div
                      key={t.id}
                      className="flex items-center justify-between rounded-lg px-2 py-1.5 text-[13px] hover:bg-peach-2"
                    >
                      <span className="text-ink">{t.name}</span>
                      <span className="flex items-center gap-2">
                        {t.built_in && <span className="text-[11px] text-mute">built-in</span>}
                        <Btn variant="ghost" size="sm" onClick={() => setEditingTpl(t)}>
                          edit
                        </Btn>
                        <Btn
                          variant="outline"
                          size="sm"
                          className="hover:text-rec"
                          onClick={() => void removeTpl(t.id)}
                        >
                          delete
                        </Btn>
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </section>

            <section className="space-y-2">
              <SectionLabel>Chat with your notes (MCP)</SectionLabel>
              <div className="rounded-[16px] bg-ink p-5 text-white">
                <p className="mb-3 text-[13px] leading-relaxed text-white/70">
                  Looma ships a local MCP server. Add this to Claude Desktop's{" "}
                  <code className="rounded bg-white/10 px-1 py-0.5 font-mono text-[11px] text-white">
                    claude_desktop_config.json
                  </code>{" "}
                  (or any MCP client) to search and read your notes and transcripts — everything
                  stays on this machine.
                </p>
                {mcpJson && (
                  <div className="relative">
                    <pre className="overflow-x-auto whitespace-pre rounded-[10px] border border-white/15 bg-white/10 p-3 font-mono text-[12px] text-white">
                      {mcpJson}
                    </pre>
                    <Btn
                      variant="primary"
                      size="sm"
                      className="absolute right-2 top-2"
                      onClick={() => {
                        void navigator.clipboard.writeText(mcpJson).then(() => {
                          setMcpCopied(true);
                          window.setTimeout(() => setMcpCopied(false), 1500);
                        });
                      }}
                    >
                      {mcpCopied ? "Copied ✓" : "Copy"}
                    </Btn>
                  </div>
                )}
              </div>
            </section>

            <section className="space-y-2">
              <SectionLabel>App updates</SectionLabel>
              {updater.supported ? (
                <div className="space-y-2 rounded-[14px] border border-line bg-surface p-4">
                  <div className="flex items-center justify-between gap-3">
                    <span className="text-[13px] text-ink-2">
                      Current version{" "}
                      <span className="font-semibold text-ink">v{appVersion ?? "…"}</span>
                    </span>
                    {(updater.phase === "idle" ||
                      updater.phase === "upToDate" ||
                      updater.phase === "error") && (
                      <Btn variant="outline" size="sm" onClick={updater.check}>
                        Check for updates
                      </Btn>
                    )}
                    {updater.phase === "checking" && (
                      <Btn variant="outline" size="sm" disabled>
                        Checking…
                      </Btn>
                    )}
                    {updater.phase === "available" && (
                      <Btn
                        variant="primary"
                        size="sm"
                        disabled={recordingActive}
                        onClick={updater.downloadAndInstall}
                      >
                        Download &amp; install v{updater.version}
                      </Btn>
                    )}
                    {updater.phase === "downloading" && (
                      <span className="flex items-center gap-2">
                        <span className="h-1.5 w-24 overflow-hidden rounded-full bg-line">
                          <span
                            className={`block h-full rounded-full bg-coral ${
                              updater.progress == null ? "w-1/3 animate-pulse" : ""
                            }`}
                            style={
                              updater.progress == null
                                ? undefined
                                : { width: `${Math.round(updater.progress * 100)}%` }
                            }
                          />
                        </span>
                        <span className="font-mono text-[11px] text-mute">
                          {updater.progress == null
                            ? "downloading…"
                            : `${Math.round(updater.progress * 100)}%`}
                        </span>
                      </span>
                    )}
                    {updater.phase === "ready" && (
                      <Btn
                        variant="primary"
                        size="sm"
                        disabled={recordingActive}
                        onClick={updater.restart}
                      >
                        Restart now
                      </Btn>
                    )}
                    {updater.phase === "installing" && (
                      <span className="text-xs text-ink-2">Installing — Looma will restart…</span>
                    )}
                  </div>
                  {updater.phase === "upToDate" && (
                    <p className="text-xs font-medium text-spk-teal">
                      You're on the latest version ✓
                    </p>
                  )}
                  {updater.phase === "error" && (
                    <p className="text-xs text-clay">✗ {updater.error}</p>
                  )}
                  {updater.phase === "ready" && !recordingActive && (
                    <p className="text-xs text-ink-2">
                      Update downloaded — restart when you're ready.
                    </p>
                  )}
                  {recordingActive &&
                    (updater.phase === "available" || updater.phase === "ready") && (
                      <p className="text-xs text-ink-2">
                        Recording in progress — the update waits until you're done.
                      </p>
                    )}
                </div>
              ) : (
                <p className="text-[11px] text-mute">
                  Auto-update is Windows-only for now — new versions are published on the GitHub
                  releases page.
                </p>
              )}
            </section>
          </div>

          <div className="flex justify-end gap-2 border-t border-line px-6 py-4">
            <Btn variant="outline" size="sm" onClick={onClose}>
              Cancel
            </Btn>
            <Btn variant="primary" size="sm" disabled={saving} onClick={() => void save()}>
              {saving ? "Saving…" : "Save"}
            </Btn>
          </div>
        </div>
      </ModalShell>
    </div>
  );
}
