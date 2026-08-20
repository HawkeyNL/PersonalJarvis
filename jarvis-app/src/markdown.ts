// Safe Markdown → HTML for Jarvis' chat replies (bold, links, images, code…).
//
// Security: the brain's output is only semi-trusted (a prompt injection could
// try to smuggle HTML), so raw HTML is NEVER interpreted — `html: false` makes
// markdown-it escape any `<script>`/`<img onerror=…>` in the source. The only
// tags produced are Markdown's own (strong/em/a/img/code/ul…), and markdown-it's
// default link validation already blocks `javascript:`/`vbscript:` URLs. Link
// clicks are intercepted in the component and opened in the system browser
// (a bare <a> would navigate the whole webview away from the app).
import MarkdownIt from "markdown-it";
import { openUrl } from "@tauri-apps/plugin-opener";

const md = new MarkdownIt({
  html: false, // the key XSS guard — never render raw HTML from the model
  linkify: true, // turn bare URLs into links
  breaks: true, // single newline → <br>, so chat keeps its line breaks
  typographer: false,
});

/** Render Markdown to sanitized HTML for `v-html`. */
export function renderMarkdown(src: string): string {
  return md.render(src ?? "");
}

/**
 * If a click landed on a link inside rendered Markdown, open it in the system
 * browser instead of navigating the webview. Returns true if it handled a link.
 */
export function handleMarkdownClick(e: MouseEvent): boolean {
  const anchor = (e.target as HTMLElement | null)?.closest("a");
  const href = anchor?.getAttribute("href");
  if (!href) return false;
  e.preventDefault();
  // Only hand off web/mail links; ignore anything exotic that slipped through.
  if (/^(https?:|mailto:)/i.test(href)) void openUrl(href);
  return true;
}
