import { useEffect, useState } from "react";
import { api } from "../api";
import type { AsrSettings, LlmSettings, ModelProgress, Template } from "../types";

interface Props {
  modelProgress: ModelProgress | null;
  onClose: () => void;
}

const TIERS = [
  { id: "light", label: "Light", desc: "Whisper small (Q5) — ~2 GB RAM, fine on old laptops" },
  { id: "balanced", label: "Balanced", desc: "large-v3-turbo (Q5) — near-large accuracy on CPU" },
  { id: "best", label: "Best", desc: "large-v3-turbo (Q5), large-v3 with max-quality toggle" },
  { id: "cloud", label: "Cloud (Groq)", desc: "For weak devices — audio LEAVES this machine" },
];

function gb(bytes: number): string {
  return bytes >= 1_000_000_000
    ? `${(bytes / 1_000_000_000).toFixed(1)} GB`
    : `${Math.round(bytes / 1_000_000)} MB`;
}

export default function SettingsModal({ modelProgress, onClose }: Props) {
  const [settings, setSettings] = useState<AsrSettings | null>(null);
  const [tier, setTier] = useState("balanced");
  const [modelId, setModelId] = useState<string>("");
  const [useGroq, setUseGroq] = useState(false);
  const [maxQuality, setMaxQuality] = useState(false);
  const [autoTranscribe, setAutoTranscribe] = useState(true);
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

  const load = () =>
    api.getAsrSettings().then((s) => {
      setSettings(s);
      setTier(s.tier);
      setModelId(s.model_id ?? "");
      setUseGroq(s.use_groq);
      setMaxQuality(s.max_quality);
      setAutoTranscribe(s.auto_transcribe);
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
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      onClick={onClose}
    >
      <div
        className="max-h-[85vh] w-[560px] overflow-y-auto rounded-lg border border-zinc-700 bg-zinc-900 p-5 text-sm text-zinc-200 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 flex items-center justify-between">
          <h2 className="text-lg font-semibold">Transcription settings</h2>
          <button className="text-zinc-500 hover:text-zinc-200" onClick={onClose}>
            ✕
          </button>
        </div>

        {settings && (
          <div className="mb-4 rounded-md bg-zinc-800/70 px-3 py-2 text-xs text-zinc-400">
            This machine: {settings.hw.cpu_cores} cores · {settings.hw.ram_gb} GB RAM
            {settings.hw.gpu_name
              ? ` · ${settings.hw.gpu_name} (${Math.round((settings.hw.vram_mb ?? 0) / 1024)} GB VRAM)`
              : " · no NVIDIA GPU"}
            {" — recommended tier: "}
            <span className="font-medium text-zinc-200 capitalize">
              {settings.hw.recommended_tier}
            </span>
          </div>
        )}

        <div className="mb-4">
          <div className="mb-1 font-medium">Hardware tier</div>
          {TIERS.map((t) => (
            <label key={t.id} className="mb-1 flex cursor-pointer items-start gap-2">
              <input
                type="radio"
                checked={tier === t.id}
                onChange={() => setTier(t.id)}
                className="mt-1"
              />
              <span>
                <span className="font-medium">{t.label}</span>
                <span className="ml-2 text-xs text-zinc-500">{t.desc}</span>
              </span>
            </label>
          ))}
        </div>

        <div className="mb-4 flex items-center gap-3">
          <div className="font-medium">Model</div>
          <select
            value={modelId}
            onChange={(e) => setModelId(e.target.value)}
            className="rounded border border-zinc-700 bg-zinc-800 px-2 py-1 text-xs"
          >
            <option value="">Auto (per tier)</option>
            {asrModels.map((m) => (
              <option key={m.id} value={m.id}>
                {m.display}
              </option>
            ))}
          </select>
        </div>

        <label className="mb-2 flex items-center gap-2">
          <input
            type="checkbox"
            checked={maxQuality}
            onChange={(e) => setMaxQuality(e.target.checked)}
          />
          Maximum quality (full large-v3 — slower, ~1 GB download)
        </label>
        <label className="mb-2 flex items-center gap-2">
          <input
            type="checkbox"
            checked={autoTranscribe}
            onChange={(e) => setAutoTranscribe(e.target.checked)}
          />
          Transcribe automatically when a recording stops
        </label>

        <div className="mb-4 mt-3 rounded-md border border-amber-900/60 bg-amber-950/30 p-3">
          <label className="flex items-center gap-2 font-medium">
            <input
              type="checkbox"
              checked={useGroq}
              onChange={(e) => setUseGroq(e.target.checked)}
            />
            Use Groq for transcription (cloud fallback)
          </label>
          <p className="mt-1 text-xs text-amber-200/80">
            When enabled, meeting audio is uploaded to Groq for transcription. Who-said-what
            (diarization) still runs locally. Bring your own key — free tier available.
          </p>
          <input
            type="password"
            placeholder={settings?.has_groq_key ? "Groq API key saved — enter to replace" : "gsk_…"}
            value={groqKey}
            onChange={(e) => setGroqKey(e.target.value)}
            className="mt-2 w-full rounded border border-zinc-700 bg-zinc-800 px-2 py-1 text-xs outline-none focus:border-indigo-500"
          />
        </div>

        <div className="mb-4">
          <div className="mb-1 font-medium">Models & engines on disk</div>
          {[...asrModels, ...otherModels].map((m) => (
            <div key={m.id} className="flex items-center justify-between py-1 text-xs">
              <span className="truncate text-zinc-400">{m.display}</span>
              <span className="ml-2 flex shrink-0 items-center gap-2">
                <span className="text-zinc-600">{gb(m.bytes)}</span>
                {m.installed ? (
                  <span className="text-emerald-400">installed</span>
                ) : downloading === m.id ? (
                  <span className="text-indigo-300">
                    {modelProgress && modelProgress.id === m.id && modelProgress.total > 0
                      ? `${Math.round((modelProgress.downloaded / modelProgress.total) * 100)}%`
                      : "downloading…"}
                  </span>
                ) : (
                  <button
                    className="rounded border border-zinc-700 px-2 py-0.5 text-zinc-300 hover:bg-zinc-800"
                    onClick={() => void download(m.id)}
                  >
                    download
                  </button>
                )}
              </span>
            </div>
          ))}
        </div>

        <div className="mb-4 border-t border-zinc-800 pt-4">
          <div className="mb-1 font-medium">AI provider (for Enhance & Ask)</div>
          <div className="mb-2 flex flex-wrap gap-2">
            {llm?.providers.map((p) => (
              <button
                key={p.id}
                onClick={() => pickProvider(p.id)}
                className={`rounded-md border px-2.5 py-1 text-xs ${
                  llmProvider === p.id
                    ? "border-indigo-500 text-indigo-200"
                    : "border-zinc-700 text-zinc-400 hover:bg-zinc-800"
                }`}
              >
                {p.id}
                <span className={`ml-1.5 ${p.is_local ? "text-emerald-400" : "text-amber-400"}`}>
                  {p.is_local ? "local" : "cloud"}
                </span>
                {p.has_key && <span className="ml-1 text-zinc-500">🔑</span>}
              </button>
            ))}
          </div>
          <div className="grid grid-cols-2 gap-2 text-xs">
            <input
              placeholder={`model (default: ${
                llm?.providers.find((p) => p.id === llmProvider)?.default_model ?? ""
              })`}
              value={llmModel}
              onChange={(e) => setLlmModel(e.target.value)}
              className="rounded border border-zinc-700 bg-zinc-800 px-2 py-1 outline-none focus:border-indigo-500"
            />
            <input
              placeholder={
                llmProvider === "ollama"
                  ? "base URL (default: localhost:11434)"
                  : "base URL (optional)"
              }
              value={llmBaseUrl}
              onChange={(e) => setLlmBaseUrl(e.target.value)}
              className="rounded border border-zinc-700 bg-zinc-800 px-2 py-1 outline-none focus:border-indigo-500"
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
                className="col-span-2 rounded border border-zinc-700 bg-zinc-800 px-2 py-1 outline-none focus:border-indigo-500"
              />
            )}
          </div>
          <div className="mt-2 flex items-center gap-2 text-xs">
            <button
              onClick={() => void saveLlm()}
              disabled={llmBusy}
              className="rounded bg-zinc-700 px-2.5 py-1 text-zinc-100 hover:bg-zinc-600 disabled:opacity-50"
            >
              Save provider
            </button>
            <button
              onClick={() => void testLlm()}
              disabled={llmBusy}
              className="rounded border border-zinc-700 px-2.5 py-1 text-zinc-300 hover:bg-zinc-800 disabled:opacity-50"
            >
              Test connection
            </button>
            {llmTest && <span className="text-zinc-400">{llmTest}</span>}
          </div>
          <p className="mt-1 text-[11px] text-zinc-500">
            Ollama runs entirely on this machine. OpenAI, Anthropic, and NVIDIA NIM send your notes
            + transcript to their APIs when you use Enhance or Ask.
          </p>
        </div>

        <div className="mb-4 border-t border-zinc-800 pt-4">
          <div className="mb-1 flex items-center justify-between font-medium">
            Note templates
            <button
              className="rounded border border-zinc-700 px-2 py-0.5 text-xs text-zinc-300 hover:bg-zinc-800"
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
            </button>
          </div>
          {editingTpl ? (
            <div className="rounded border border-zinc-700 p-2 text-xs">
              <input
                value={editingTpl.name}
                onChange={(e) => setEditingTpl({ ...editingTpl, name: e.target.value })}
                className="mb-1 w-full rounded border border-zinc-700 bg-zinc-800 px-2 py-1 outline-none"
                placeholder="Template name"
              />
              <textarea
                value={editingTpl.system_prompt}
                onChange={(e) => setEditingTpl({ ...editingTpl, system_prompt: e.target.value })}
                rows={3}
                className="mb-1 w-full rounded border border-zinc-700 bg-zinc-800 px-2 py-1 outline-none"
                placeholder="System prompt"
              />
              <textarea
                value={editingTpl.structure_hint}
                onChange={(e) => setEditingTpl({ ...editingTpl, structure_hint: e.target.value })}
                rows={3}
                className="mb-1 w-full rounded border border-zinc-700 bg-zinc-800 px-2 py-1 font-mono outline-none"
                placeholder="Structure hint (markdown headings)"
              />
              <div className="flex gap-2">
                <button
                  onClick={() => void saveTpl()}
                  className="rounded bg-indigo-600 px-2 py-0.5 text-white hover:bg-indigo-500"
                >
                  Save template
                </button>
                <button
                  onClick={() => setEditingTpl(null)}
                  className="rounded border border-zinc-700 px-2 py-0.5 text-zinc-400"
                >
                  Cancel
                </button>
              </div>
            </div>
          ) : (
            templates.map((t) => (
              <div key={t.id} className="flex items-center justify-between py-1 text-xs">
                <span className="text-zinc-300">{t.name}</span>
                <span className="flex items-center gap-2">
                  {t.built_in && <span className="text-zinc-600">built-in</span>}
                  <button
                    className="text-zinc-400 hover:text-zinc-200"
                    onClick={() => setEditingTpl(t)}
                  >
                    edit
                  </button>
                  <button
                    className="text-zinc-500 hover:text-red-400"
                    onClick={() => void removeTpl(t.id)}
                  >
                    delete
                  </button>
                </span>
              </div>
            ))
          )}
        </div>

        <div className="flex justify-end gap-2">
          <button
            className="rounded border border-zinc-700 px-3 py-1.5 text-zinc-300 hover:bg-zinc-800"
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            className="rounded bg-indigo-600 px-3 py-1.5 font-medium text-white hover:bg-indigo-500 disabled:opacity-50"
            disabled={saving}
            onClick={() => void save()}
          >
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
