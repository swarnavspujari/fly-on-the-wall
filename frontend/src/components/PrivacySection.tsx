import { useEffect, useState } from "react";
import { relaunch } from "@tauri-apps/plugin-process";
import { ShieldAlert, X } from "lucide-react";
import { api } from "../api";
import {
  ENCRYPTION_BANNER_DISMISSED_KEY,
  WIPE_CONFIRMATION,
  canConfirmWipe,
  encryptionBannerText,
  formatBytes,
  parseRetention,
  retentionOptions,
  retentionWarning,
  showEncryptionBanner,
} from "../privacy";
import type { DiskEncryption, PrivacySettings, WipeReport } from "../types";
import { Button, Card, Checkbox, Input, SectionLabel, Select } from "./ui";

/** Settings → Privacy: disk-encryption notice, audio retention, and the
 *  "Delete all my data" danger zone. Every control saves on its own (no
 *  dependency on the modal's Save button). */
export default function PrivacySection({ recordingActive }: { recordingActive: boolean }) {
  const [settings, setSettings] = useState<PrivacySettings | null>(null);
  const [encryption, setEncryption] = useState<DiskEncryption | null>(null);
  const [bannerDismissed, setBannerDismissed] = useState<string | null>("true");

  // retention: a change that deletes audio right away is confirmed first
  const [pending, setPending] = useState<{ days: number; warning: string } | null>(null);
  const [retentionMsg, setRetentionMsg] = useState<string | null>(null);
  const [retentionBusy, setRetentionBusy] = useState(false);

  // delete all: step 1 = open, step 2 = acknowledged + typed phrase
  const [wipeOpen, setWipeOpen] = useState(false);
  const [wipeAck, setWipeAck] = useState(false);
  const [wipeTyped, setWipeTyped] = useState("");
  const [wipeModels, setWipeModels] = useState(false);
  const [wipeBusy, setWipeBusy] = useState(false);
  const [wipeError, setWipeError] = useState<string | null>(null);
  const [wipeDone, setWipeDone] = useState<WipeReport | null>(null);

  useEffect(() => {
    void api
      .getPrivacySettings()
      .then(setSettings)
      .catch(() => {});
    void api
      .diskEncryptionStatus()
      .then(setEncryption)
      .catch(() => {});
    void api
      .getAppSetting(ENCRYPTION_BANNER_DISMISSED_KEY)
      .then(setBannerDismissed)
      .catch(() => setBannerDismissed(null));
  }, []);

  const choices = settings?.retention_choices ?? [7, 30, 90, 365];

  const applyRetention = async (days: number | null) => {
    setRetentionBusy(true);
    setRetentionMsg(null);
    try {
      const r = await api.setAudioRetention(days);
      setSettings((s) => (s ? { ...s, audio_retention_days: days } : s));
      setRetentionMsg(
        r.meetings > 0
          ? `Deleted audio from ${r.meetings} meeting${r.meetings === 1 ? "" : "s"} (${formatBytes(r.bytes_removed)} freed).`
          : "Saved ✓",
      );
      if (r.meetings_failed > 0) {
        setRetentionMsg(
          (m) =>
            `${m ?? ""} ${r.meetings_failed} couldn't be deleted (files in use) — retried tomorrow.`,
        );
      }
    } catch (e) {
      setRetentionMsg(String(e));
    } finally {
      setRetentionBusy(false);
      setPending(null);
    }
  };

  const onRetentionChange = async (value: string) => {
    const days = parseRetention(value, choices);
    if (days === null) {
      await applyRetention(null);
      return;
    }
    try {
      const affected = await api.previewAudioRetention(days);
      const warning = retentionWarning(days, affected);
      if (warning) setPending({ days, warning });
      else await applyRetention(days);
    } catch (e) {
      setRetentionMsg(String(e));
    }
  };

  const runWipe = async () => {
    setWipeBusy(true);
    setWipeError(null);
    try {
      const report = await api.deleteAllData(wipeModels, WIPE_CONFIRMATION);
      // per-viewer UI state (theme, folder colors, last view) goes too
      try {
        localStorage.clear();
      } catch {
        /* storage may be unavailable */
      }
      setWipeDone(report);
      // Nothing stuck → restart straight away; otherwise let the user read
      // the leftovers first (the restart still finishes the job).
      if (report.failed.length === 0 && report.secrets_failed.length === 0) {
        window.setTimeout(() => void relaunch(), 1500);
      }
    } catch (e) {
      setWipeError(String(e));
    } finally {
      setWipeBusy(false);
    }
  };

  const closeWipe = () => {
    setWipeOpen(false);
    setWipeAck(false);
    setWipeTyped("");
    setWipeModels(false);
    setWipeError(null);
  };

  return (
    <section className="space-y-3">
      <SectionLabel style={{ display: "block", marginBottom: 8 }}>Privacy</SectionLabel>

      {encryption && showEncryptionBanner(encryption, bannerDismissed) && (
        <div
          role="status"
          className="flex items-start gap-2.5 rounded-xl border border-line bg-warning-soft px-3.5 py-3 text-[12.5px] leading-[1.5] text-text"
        >
          <ShieldAlert
            size={16}
            strokeWidth={1.75}
            className="mt-[1px] flex-none"
            style={{ color: "var(--warning-text)" }}
          />
          <span className="min-w-0 flex-1">{encryptionBannerText(encryption)}</span>
          <button
            title="Dismiss"
            aria-label="Dismiss disk encryption notice"
            className="flex-none cursor-pointer rounded p-0.5 text-text-3 hover:text-text"
            onClick={() => {
              setBannerDismissed("true");
              void api.setAppSetting(ENCRYPTION_BANNER_DISMISSED_KEY, "true");
            }}
          >
            <X size={14} strokeWidth={2} />
          </button>
        </div>
      )}

      {settings?.last_wipe && (
        <div className="flex items-start gap-2.5 rounded-xl border border-line bg-surface-2 px-3.5 py-3 text-[12.5px] leading-[1.5] text-text-2">
          <span className="min-w-0 flex-1">
            The last “Delete all my data” couldn't remove {settings.last_wipe.failed.length} item
            {settings.last_wipe.failed.length === 1 ? "" : "s"} (another program had them open):{" "}
            <span style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}>
              {settings.last_wipe.failed.slice(0, 5).join(", ")}
              {settings.last_wipe.failed.length > 5 ? "…" : ""}
            </span>
            . Delete them from the data folder by hand.
          </span>
          <button
            title="Dismiss"
            className="flex-none cursor-pointer rounded p-0.5 text-text-3 hover:text-text"
            onClick={() => {
              setSettings((s) => (s ? { ...s, last_wipe: null } : s));
              void api.dismissWipeReport();
            }}
          >
            <X size={14} strokeWidth={2} />
          </button>
        </div>
      )}

      {/* Retention */}
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0">
          <span className="text-text" style={{ fontSize: 13, fontWeight: 500 }}>
            Delete recording audio
          </span>
          <p className="text-text-3" style={{ margin: "2px 0 0", fontSize: 11.5, lineHeight: 1.5 }}>
            Transcripts and notes are always kept. Checked on startup and daily.
          </p>
        </div>
        <Select
          aria-label="Delete recording audio after"
          style={{ maxWidth: "12rem" }}
          disabled={!settings || retentionBusy}
          value={pending ? String(pending.days) : String(settings?.audio_retention_days ?? "")}
          onChange={(e) => void onRetentionChange(e.target.value)}
          options={retentionOptions(choices)}
        />
      </div>
      {pending && (
        <Card tone="muted" pad="sm">
          <p className="text-text" style={{ margin: "0 0 8px", fontSize: 12.5, lineHeight: 1.5 }}>
            {pending.warning}
          </p>
          <div className="flex gap-2">
            <Button
              variant="contrast"
              size="sm"
              disabled={retentionBusy}
              onClick={() => void applyRetention(pending.days)}
            >
              {retentionBusy ? "Deleting…" : "Delete audio"}
            </Button>
            <Button variant="outline" size="sm" onClick={() => setPending(null)}>
              Cancel
            </Button>
          </div>
        </Card>
      )}
      {retentionMsg && (
        <p className="text-text-2" style={{ margin: 0, fontSize: 12 }}>
          {retentionMsg}
        </p>
      )}

      {/* Danger zone */}
      <div
        className="rounded-xl px-3.5 py-3"
        style={{ border: "1px solid var(--error)", background: "var(--surface)" }}
      >
        <div className="flex items-center justify-between gap-3">
          <div className="min-w-0">
            <span className="text-error-text" style={{ fontSize: 13, fontWeight: 600 }}>
              Delete all my data
            </span>
            <p
              className="text-text-3"
              style={{ margin: "2px 0 0", fontSize: 11.5, lineHeight: 1.5 }}
            >
              Erases every recording, transcript, note, attachment, setting and log on this
              computer, plus saved API keys and calendar sign-ins. Then the app restarts.
            </p>
          </div>
          {!wipeOpen && !wipeDone && (
            <Button
              variant="outline"
              size="sm"
              className="flex-none"
              style={{ color: "var(--error-text)", borderColor: "var(--error)" }}
              disabled={recordingActive}
              title={recordingActive ? "Stop the recording first" : undefined}
              onClick={() => setWipeOpen(true)}
            >
              Delete all…
            </Button>
          )}
        </div>

        {wipeOpen && !wipeDone && (
          <div className="space-y-2.5" style={{ marginTop: 12 }}>
            <Checkbox
              checked={wipeAck}
              onChange={(e) => setWipeAck(e.target.checked)}
              label="I understand this can't be undone"
              description="There's no backup. Export anything you want to keep first."
            />
            <Checkbox
              checked={wipeModels}
              onChange={(e) => setWipeModels(e.target.checked)}
              label="Also delete downloaded models and engines"
              description="Frees disk space; they download again when needed."
            />
            <Input
              label={`Type "${WIPE_CONFIRMATION}" to confirm`}
              value={wipeTyped}
              autoComplete="off"
              spellCheck={false}
              onChange={(e) => setWipeTyped(e.target.value)}
            />
            {wipeError && (
              <p className="text-error-text" style={{ margin: 0, fontSize: 12 }}>
                ✗ {wipeError}
              </p>
            )}
            <div className="flex gap-2">
              <Button
                variant="record"
                size="sm"
                disabled={!canConfirmWipe(wipeAck, wipeTyped) || wipeBusy || recordingActive}
                onClick={() => void runWipe()}
              >
                {wipeBusy ? "Deleting…" : "Delete everything"}
              </Button>
              <Button variant="outline" size="sm" disabled={wipeBusy} onClick={closeWipe}>
                Cancel
              </Button>
            </div>
          </div>
        )}

        {wipeDone && (
          <div className="space-y-2" style={{ marginTop: 12 }}>
            {wipeDone.failed.length === 0 && wipeDone.secrets_failed.length === 0 ? (
              <p className="text-text-2" style={{ margin: 0, fontSize: 12.5 }}>
                Deleted. Restarting to finish…
              </p>
            ) : (
              <>
                <p className="text-text" style={{ margin: 0, fontSize: 12.5, lineHeight: 1.5 }}>
                  Most data is deleted. Some items are in use and will be removed when the app
                  restarts
                  {wipeDone.secrets_failed.length > 0
                    ? `; these keychain entries couldn't be removed: ${wipeDone.secrets_failed.join(", ")}`
                    : ""}
                  .
                </p>
                {wipeDone.failed.length > 0 && (
                  <p
                    className="text-text-3"
                    style={{ margin: 0, fontFamily: "var(--font-mono)", fontSize: 11 }}
                  >
                    {wipeDone.failed.slice(0, 8).join(", ")}
                    {wipeDone.failed.length > 8 ? "…" : ""}
                  </p>
                )}
                <Button variant="record" size="sm" onClick={() => void relaunch()}>
                  Restart now
                </Button>
              </>
            )}
          </div>
        )}
      </div>
    </section>
  );
}
