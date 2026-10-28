import type { ReactNode } from "react";
import { useEffect, useState } from "react";
import { ShieldAlert, X } from "lucide-react";
import { api } from "../api";
import {
  ENCRYPTION_BANNER_DISMISSED_KEY,
  RESTART_REQUIRED_EVENT,
  WIPE_CONFIRMATION,
  canConfirmWipe,
  encryptionBannerText,
  formatBytes,
  parseRetention,
  retentionOptions,
  retentionWarning,
  showEncryptionBanner,
  type RestartRequiredDetail,
} from "../privacy";
import type { DiskEncryption, PrivacySettings } from "../types";
import { Button, Card, Checkbox, Input, Select, SettingsGroup, SettingsRow } from "./ui";

/** Settings › Your files and privacy: the disk-encryption notice (top row),
 *  the caller's rows (`children`: file location, search index), audio
 *  retention, and "Delete all my data" (last row, two-step). Every control
 *  saves on its own; none waits for the Transcripts save bar. */
export default function PrivacySection({
  recordingActive,
  children,
}: {
  recordingActive: boolean;
  children?: ReactNode;
}) {
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
      setSettings((s) => (s ? { ...s, audio_retention_days: days, retention_held: null } : s));
      setRetentionMsg(
        r.meetings > 0
          ? `Deleted audio from ${r.meetings} meeting${r.meetings === 1 ? "" : "s"} (${formatBytes(r.bytes_removed)} freed).`
          : "Saved.",
      );
      if (r.meetings_failed > 0) {
        setRetentionMsg(
          (m) =>
            `${m ?? ""} ${r.meetings_failed} couldn't be deleted (files in use). They're retried tomorrow.`,
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
      // From here the app must restart before anything else happens: the
      // App-level RestartRequired screen takes over (outlives Settings).
      window.dispatchEvent(
        new CustomEvent<RestartRequiredDetail>(RESTART_REQUIRED_EVENT, {
          detail: { report, includeModels: wipeModels },
        }),
      );
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

  const lastWipe = settings?.last_wipe ?? null;

  return (
    <SettingsGroup>
      {encryption && showEncryptionBanner(encryption, bannerDismissed) && (
        <SettingsRow
          role="status"
          style={{ background: "var(--warning-soft)" }}
          title={
            <>
              <ShieldAlert
                size={16}
                strokeWidth={1.75}
                aria-hidden="true"
                style={{ color: "var(--warning-text)" }}
              />
              Disk encryption looks off
            </>
          }
          helper={encryptionBannerText(encryption)}
          control={
            <Button
              variant="ghost"
              size="sm"
              aria-label="Dismiss disk encryption notice"
              onClick={() => {
                setBannerDismissed("true");
                void api.setAppSetting(ENCRYPTION_BANNER_DISMISSED_KEY, "true");
              }}
            >
              Dismiss
            </Button>
          }
        />
      )}

      {children}

      <SettingsRow
        title="Delete audio after"
        helper="Transcripts and notes are always kept. Checked on startup and daily."
        control={
          <Select
            aria-label="Delete recording audio after"
            style={{ width: 190 }}
            disabled={!settings || retentionBusy}
            value={pending ? String(pending.days) : String(settings?.audio_retention_days ?? "")}
            onChange={(e) => void onRetentionChange(e.target.value)}
            options={retentionOptions(choices)}
          />
        }
      >
        {(pending ||
          (settings?.retention_held != null && settings.audio_retention_days != null) ||
          retentionMsg) && (
          <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            {pending && (
              <Card tone="muted" pad="sm">
                <p
                  className="text-text"
                  style={{ margin: "0 0 8px", fontSize: 12.5, lineHeight: 1.5 }}
                >
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
            {settings?.retention_held != null &&
              settings.audio_retention_days != null &&
              !pending && (
                <Card tone="muted" pad="sm">
                  <p
                    className="text-text"
                    style={{ margin: "0 0 8px", fontSize: 12.5, lineHeight: 1.5 }}
                  >
                    Retention is paused: {settings.retention_held} meetings would lose their audio
                    at once, which can mean the clock changed. If that's expected, confirm to delete
                    it.
                  </p>
                  <Button
                    variant="contrast"
                    size="sm"
                    disabled={retentionBusy}
                    onClick={() => void applyRetention(settings.audio_retention_days)}
                  >
                    {retentionBusy ? "Deleting…" : "Delete audio"}
                  </Button>
                </Card>
              )}
            {retentionMsg && (
              <p role="status" className="text-text-2" style={{ margin: 0, fontSize: 12 }}>
                {retentionMsg}
              </p>
            )}
          </div>
        )}
      </SettingsRow>

      <SettingsRow
        tone="danger"
        title="Delete all my data"
        helper="Erases every recording, transcript, note, attachment, setting and log on this computer, plus saved API keys and calendar sign-ins. Then the app restarts."
        control={
          !wipeOpen && (
            <Button
              variant="outline"
              size="sm"
              style={{ color: "var(--error-text)", borderColor: "var(--error)" }}
              disabled={recordingActive}
              title={recordingActive ? "Stop the recording first" : undefined}
              onClick={() => setWipeOpen(true)}
            >
              Delete all…
            </Button>
          )
        }
      >
        {(lastWipe || wipeOpen) && (
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            {lastWipe && (
              <div className="flex items-start gap-2.5 rounded-xl border border-line bg-surface-2 px-3.5 py-3 text-[12.5px] leading-[1.5] text-text-2">
                <span className="min-w-0 flex-1">
                  {lastWipe.notes.map((n) => (
                    <span key={n} className="block">
                      {n}
                    </span>
                  ))}
                  {lastWipe.failed.length > 0 && (
                    <>
                      The last “Delete all my data” couldn't remove {lastWipe.failed.length} item
                      {lastWipe.failed.length === 1 ? "" : "s"} (another program had them open):{" "}
                      <span style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}>
                        {lastWipe.failed.slice(0, 5).join(", ")}
                        {lastWipe.failed.length > 5 ? "…" : ""}
                      </span>
                      . Delete them from the data folder by hand.
                    </>
                  )}
                </span>
                <button
                  type="button"
                  title="Dismiss"
                  aria-label="Dismiss the last delete report"
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

            {wipeOpen && (
              <div className="space-y-2.5">
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
                    {wipeError}
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
          </div>
        )}
      </SettingsRow>
    </SettingsGroup>
  );
}
