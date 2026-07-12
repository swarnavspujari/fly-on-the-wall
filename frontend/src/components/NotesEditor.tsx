import { useEffect, useRef } from "react";
import { marked } from "marked";
import DOMPurify from "dompurify";
import TurndownService from "turndown";
import { gfm } from "turndown-plugin-gfm";
import {
  Bold,
  Heading2,
  Highlighter,
  Italic,
  Link2,
  List,
  ListChecks,
  ListOrdered,
  Quote,
} from "lucide-react";

/* Rich-text notes: a WYSIWYG contentEditable whose content is stored as
   MARKDOWN (so Enhance / export / search stay clean). Markdown → HTML on load
   via marked; HTML → markdown on edit via turndown (+gfm for task lists), with
   <mark> kept as inline HTML for highlights. Uncontrolled: React never
   re-renders the editable node, so the caret never jumps — content only
   reloads when `revision` changes (note switch or an external insert). */

marked.setOptions({ gfm: true, breaks: false });

const td = new TurndownService({
  headingStyle: "atx",
  bulletListMarker: "-",
  codeBlockStyle: "fenced",
  emDelimiter: "*",
});
td.use(gfm);
td.keep(["mark"]); // preserve highlights as inline HTML in the markdown

function mdToHtml(md: string): string {
  const html = (marked.parse(md ?? "") as string)
    // Make task-list checkboxes editable (marked emits them disabled).
    .replace(/<input([^>]*?)\sdisabled([^>]*)>/gi, "<input$1$2>");
  // Sanitize before it ever touches innerHTML — scratchpad can carry imported
  // or AI-generated content, and this webview holds the Tauri IPC surface.
  return DOMPurify.sanitize(html);
}

const RT_CSS = `
.fotw-notes{font-family:var(--font-sans);font-size:15px;line-height:1.72;color:var(--text);outline:none;caret-color:var(--primary);min-height:340px}
.fotw-notes[data-empty]:before{content:attr(data-placeholder);color:var(--text-3);pointer-events:none}
.fotw-notes>*:first-child{margin-top:0}
.fotw-notes h2{font-family:var(--font-display);font-size:19px;font-weight:700;letter-spacing:-0.01em;margin:26px 0 8px;color:var(--text)}
.fotw-notes p{margin:0 0 11px}
.fotw-notes ul,.fotw-notes ol{margin:0 0 13px;padding-left:22px}
.fotw-notes li{margin:4px 0}
.fotw-notes a{color:var(--primary-text);text-decoration:underline;text-underline-offset:2px}
.fotw-notes strong{font-weight:700}
.fotw-notes mark{background:var(--highlight);color:var(--on-highlight);padding:0 2px;border-radius:3px}
.fotw-notes blockquote{margin:14px 0;padding:6px 0 6px 15px;border-left:2px solid var(--primary-border);color:var(--text-2);font-style:italic}
.fotw-notes ul.contains-task-list,.fotw-notes ul.tasks{list-style:none;padding-left:2px}
.fotw-notes li.task-list-item,.fotw-notes li.task{display:flex;gap:9px;align-items:flex-start;margin:6px 0}
.fotw-notes input[type=checkbox]{width:16px;height:16px;margin-top:2px;flex:none;accent-color:var(--primary);cursor:pointer}
.fotw-fmtbar{display:flex;flex-wrap:wrap;align-items:center;gap:2px;border-bottom:1px solid var(--line);background:var(--surface);padding:6px 24px}
.fotw-fmt{display:grid;place-items:center;width:30px;height:28px;flex:none;border:none;border-radius:var(--radius-sm);cursor:pointer;background:transparent;color:var(--text-2);font-family:var(--font-sans);transition:background .12s,color .12s}
.fotw-fmt:hover{background:var(--surface-3);color:var(--text)}
.fotw-fmt-sep{width:1px;height:18px;background:var(--line);margin:0 5px;flex:none}
`;
if (typeof document !== "undefined" && !document.getElementById("fotw-richtext-css")) {
  const el = document.createElement("style");
  el.id = "fotw-richtext-css";
  el.textContent = RT_CSS;
  document.head.appendChild(el);
}

function FmtBtn({
  title,
  onDo,
  children,
}: {
  title: string;
  onDo: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      title={title}
      aria-label={title}
      className="fotw-fmt"
      onMouseDown={(e) => {
        e.preventDefault();
        onDo();
      }}
    >
      {children}
    </button>
  );
}

interface Props {
  markdown: string;
  /** Bumped on note switch or external insert to reload the editable content. */
  revision: number;
  onChange: (markdown: string) => void;
  placeholder?: string;
}

export default function NotesEditor({ markdown, revision, onChange, placeholder }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const saveTimer = useRef<number | null>(null);

  const refreshEmpty = () => {
    const n = ref.current;
    if (!n) return;
    const empty = !n.textContent?.trim() && !n.querySelector("img,ul,ol,h2,blockquote");
    if (empty) n.setAttribute("data-empty", "");
    else n.removeAttribute("data-empty");
  };

  // Load / reload editable content only when the note (or an insert) changes,
  // never on the user's own keystrokes — so the caret stays put.
  useEffect(() => {
    const n = ref.current;
    if (!n) return;
    n.innerHTML = mdToHtml(markdown);
    refreshEmpty();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [revision]);

  const emit = () => {
    const n = ref.current;
    if (!n) return;
    refreshEmpty();
    if (saveTimer.current !== null) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => onChange(td.turndown(n.innerHTML).trim()), 400);
  };

  const exec = (cmd: string, val?: string) => {
    ref.current?.focus();
    if (cmd === "__link") {
      let url: string | null = null;
      try {
        url = window.prompt("Link URL", "https://");
      } catch {
        /* ignore */
      }
      if (url) document.execCommand("createLink", false, url);
    } else if (cmd === "__task") {
      document.execCommand(
        "insertHTML",
        false,
        '<ul class="contains-task-list"><li class="task-list-item"><input type="checkbox"> To-do</li></ul>',
      );
    } else if (cmd === "__mark") {
      const sel = window.getSelection();
      if (sel && sel.rangeCount && !sel.getRangeAt(0).collapsed) {
        const r = sel.getRangeAt(0);
        const mark = document.createElement("mark");
        try {
          r.surroundContents(mark);
        } catch {
          document.execCommand("insertHTML", false, `<mark>${sel.toString()}</mark>`);
        }
      }
    } else {
      document.execCommand(cmd, false, val);
    }
    emit();
  };

  const sep = <span className="fotw-fmt-sep" aria-hidden="true" />;

  return (
    <div>
      <div className="fotw-fmtbar print:hidden">
        <div className="mx-auto flex w-full max-w-[var(--content-max)] flex-wrap items-center gap-0.5">
          <FmtBtn title="Bold" onDo={() => exec("bold")}>
            <Bold size={15} strokeWidth={2.25} />
          </FmtBtn>
          <FmtBtn title="Italic" onDo={() => exec("italic")}>
            <Italic size={15} strokeWidth={2.25} />
          </FmtBtn>
          <FmtBtn title="Heading" onDo={() => exec("formatBlock", "<h2>")}>
            <Heading2 size={17} strokeWidth={2} />
          </FmtBtn>
          {sep}
          <FmtBtn title="Bulleted list" onDo={() => exec("insertUnorderedList")}>
            <List size={16} strokeWidth={1.75} />
          </FmtBtn>
          <FmtBtn title="Numbered list" onDo={() => exec("insertOrderedList")}>
            <ListOrdered size={16} strokeWidth={1.75} />
          </FmtBtn>
          <FmtBtn title="Checklist" onDo={() => exec("__task")}>
            <ListChecks size={16} strokeWidth={1.75} />
          </FmtBtn>
          {sep}
          <FmtBtn title="Highlight" onDo={() => exec("__mark")}>
            <Highlighter size={16} strokeWidth={1.75} />
          </FmtBtn>
          <FmtBtn title="Quote" onDo={() => exec("formatBlock", "<blockquote>")}>
            <Quote size={16} strokeWidth={1.75} />
          </FmtBtn>
          <FmtBtn title="Add link" onDo={() => exec("__link")}>
            <Link2 size={15} strokeWidth={1.75} />
          </FmtBtn>
        </div>
      </div>
      <div className="mx-auto max-w-[var(--content-max)] px-6 pb-28 pt-5">
        <div
          ref={ref}
          className="fotw-notes"
          contentEditable
          suppressContentEditableWarning
          spellCheck
          data-placeholder={placeholder}
          onInput={emit}
          onClick={(e) => {
            const t = e.target as HTMLElement;
            if (t.matches?.('input[type="checkbox"]')) {
              if ((t as HTMLInputElement).checked) t.setAttribute("checked", "");
              else t.removeAttribute("checked");
              emit();
            }
          }}
        />
      </div>
    </div>
  );
}
