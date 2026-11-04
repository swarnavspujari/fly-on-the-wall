import type { CSSProperties } from "react";
import {
  choiceAction,
  choiceState,
  heldBackNote,
  measuredLine,
  memoryLine,
  offerAction,
  offerTitle,
  rowFor,
  sizeLabel,
  slowerLine,
} from "../localModels";
import { openExternal } from "../safeContent";
import type { LocalModelAdvice, OllamaModel } from "../types";
import { Badge, Button, Card } from "./ui";

const SMALL: CSSProperties = { margin: 0, fontSize: 12, lineHeight: 1.5 };

/** The honest label under every set of numbers: one laptop, other work
 *  running, and where it's written up. */
function MeasuredCaption({ advice }: { advice: LocalModelAdvice }) {
  return (
    <p className="text-text-3" style={{ ...SMALL, fontSize: 11 }}>
      {advice.measured_on}{" "}
      <a
        href={advice.doc_url}
        onClick={(e) => {
          e.preventDefault();
          openExternal(advice.doc_url);
        }}
        style={{ textDecoration: "underline" }}
      >
        How this was measured
      </a>
    </p>
  );
}

interface OfferProps {
  advice: LocalModelAdvice;
  installed: OllamaModel[] | undefined;
  busy: boolean;
  onAccept: (model: string) => void;
  onDismiss: (model: string) => void;
}

/** The one-time offer for an existing install: a better local model for this
 *  computer. Nothing downloads or switches until "Download and switch". */
export function LocalModelOffer({ advice, installed, busy, onAccept, onDismiss }: OfferProps) {
  const offer = advice.offer;
  if (!offer) return null;
  const row = rowFor(advice, offer.model);
  return (
    <Card tone="muted" pad="md" role="region" aria-label="Local model offer">
      <p className="text-text" style={{ ...SMALL, fontSize: 13, fontWeight: 600 }}>
        {offerTitle(offer.model, offer.download_bytes)}
      </p>
      {row && (
        <>
          <p className="mt-1 text-text-2" style={SMALL}>
            {measuredLine(row.measured)}
          </p>
          <p className="mt-1 text-text-3" style={SMALL}>
            {memoryLine(row)} You keep using your current model until you switch.
          </p>
          {slowerLine(row.measured) && (
            <p className="mt-1 text-text-3" style={SMALL}>
              {slowerLine(row.measured)}
            </p>
          )}
        </>
      )}
      <MeasuredCaption advice={advice} />
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <Button variant="primary" size="sm" disabled={busy} onClick={() => onAccept(offer.model)}>
          {offerAction(offer, installed, busy)}
        </Button>
        <Button variant="ghost" size="sm" disabled={busy} onClick={() => onDismiss(offer.model)}>
          Not now
        </Button>
      </div>
    </Card>
  );
}

interface ChoicesProps {
  advice: LocalModelAdvice;
  installed: OllamaModel[] | undefined;
  /** The model Ask and Enhance use right now. */
  effective: string;
  busy: boolean;
  onUse: (model: string) => void;
}

/** Where the user picks a local model, with what was measured for each. */
export function LocalModelChoices({ advice, installed, effective, busy, onUse }: ChoicesProps) {
  return (
    <details className="fly-model-choices">
      <summary className="cursor-pointer text-text-2" style={{ fontSize: 12 }}>
        Choose a local model
      </summary>
      <div className="mt-2 space-y-3">
        {advice.models.map((row) => {
          const state = choiceState(row, installed, effective);
          return (
            <div key={row.model} data-model={row.model}>
              <div className="flex flex-wrap items-center gap-2">
                <span
                  className="text-text"
                  style={{ fontFamily: "var(--font-mono)", fontSize: 12.5 }}
                >
                  {row.model}
                </span>
                <span className="text-text-3" style={{ fontSize: 11 }}>
                  {sizeLabel(row.download_bytes)} download
                </span>
                {row.recommended && (
                  <Badge tone="primary" size="sm">
                    recommended for this computer
                  </Badge>
                )}
                {state === "in_use" && (
                  <Badge tone="success" dot>
                    in use
                  </Badge>
                )}
              </div>
              <p className="mt-0.5 text-text-2" style={SMALL}>
                {measuredLine(row.measured)}
              </p>
              <p className="text-text-3" style={SMALL}>
                {memoryLine(row)}
                {!row.fits && " This computer has less memory than that."}
              </p>
              {slowerLine(row.measured) && (
                <p className="text-text-3" style={SMALL}>
                  {slowerLine(row.measured)}
                </p>
              )}
              {row.needs_newer_ollama && (
                <p className="text-warning-text" style={SMALL}>
                  Needs Ollama {row.needs_newer_ollama} or newer
                  {advice.ollama_version ? ` (this computer has ${advice.ollama_version})` : ""}.
                </p>
              )}
              {state !== "in_use" && !row.needs_newer_ollama && (
                <div className="mt-1">
                  <Button
                    variant="outline"
                    size="xs"
                    disabled={busy}
                    onClick={() => onUse(row.model)}
                  >
                    {choiceAction(row, state, busy)}
                  </Button>
                </div>
              )}
            </div>
          );
        })}
        {advice.held_back && (
          <p className="text-warning-text" role="status" style={SMALL}>
            {heldBackNote(advice.held_back)}
          </p>
        )}
        <MeasuredCaption advice={advice} />
      </div>
    </details>
  );
}
