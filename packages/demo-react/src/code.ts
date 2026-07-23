/**
 * Show real source on the page.
 *
 * The snippets are pulled from the actual modules with Vite's `?raw`, so the
 * code a visitor reads is the code that just ran — it can't drift the way a
 * hand-copied example does. `region()` narrows a file to the interesting
 * part using `#region` markers, which also fold in editors.
 */

/** Extract the lines between `// #region <name>` and `// #endregion`. */
export function region(source: string, name: string): string {
  const lines = source.split('\n');
  const start = lines.findIndex((l) => l.includes(`#region ${name}`));
  if (start < 0) return source.trim();
  const end = lines.findIndex((l, i) => i > start && l.includes('#endregion'));
  const body = lines.slice(start + 1, end < 0 ? undefined : end);

  // Drop the common leading indentation so an extracted block isn't
  // pushed off to the right.
  const indent = body
    .filter((l) => l.trim())
    .reduce((min, l) => Math.min(min, l.length - l.trimStart().length), Infinity);
  return body
    .map((l) => l.slice(Number.isFinite(indent) ? indent : 0))
    .join('\n')
    .trim();
}

const KEYWORDS =
  /\b(?:const|let|var|function|return|await|async|import|export|from|type|interface|new|if|else|throw|class)\b/;

const escapeHtml = (s: string): string =>
  s.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[c] as string);

/** The SGR codes the engine's help renderer actually emits. */
const SGR: Record<string, string> = { '1': 'hb', '2': 'hd', '36': 'hc' };

/**
 * Render the engine's own ANSI output as HTML.
 *
 * Used to put real `--help` text on the page: it's produced by asking the
 * live engine, so it reflects the signature that was actually registered
 * rather than a screenshot that goes stale the moment a flag is added.
 */
export function ansiToHtml(text: string): string {
  // Odd indices are the captured code lists, even indices the literal runs.
  const parts = text.split(/\u001b\[([0-9;]*)m/);
  let out = '';
  let open = 0;
  parts.forEach((part, i) => {
    if (i % 2 === 0) {
      out += escapeHtml(part);
      return;
    }
    for (const code of part.split(';')) {
      if (code === '0' || code === '') {
        out += '</i>'.repeat(open);
        open = 0;
      } else if (SGR[code]) {
        out += `<i class="${SGR[code]}">`;
        open++;
      }
    }
  });
  return out + '</i>'.repeat(open);
}

/**
 * Minimal highlighter: comments, strings, keywords. One pass with an
 * alternation so precedence is correct — a `//` inside a string stays part
 * of the string, and keywords inside comments aren't re-marked.
 */
export function highlight(code: string): string {
  return escapeHtml(code).replace(
    new RegExp(`(//[^\\n]*)|('[^'\\n]*'|"[^"\\n]*"|\`[^\`]*\`)|(${KEYWORDS.source})`, 'g'),
    (m, comment, str) =>
      comment ? `<i class="c">${m}</i>` : str ? `<i class="s">${m}</i>` : `<i class="k">${m}</i>`,
  );
}

/** Build a labelled, highlighted code panel. */
export function codePanel(title: string, source: string, name: string): HTMLElement {
  return panel(title, 'code', highlight(region(source, name)));
}

/**
 * Show what `<command> --help` prints, asked of the live engine.
 *
 * Nothing registers `--help`: the evaluator intercepts it before binding and
 * renders the signature, so the page below is generated from the same
 * metadata the command was declared with.
 */
export function helpPanel(title: string, ansi: string): HTMLElement {
  return panel(title, 'code help', ansiToHtml(ansi.trimEnd()));
}

function panel(title: string, className: string, html: string): HTMLElement {
  const wrap = document.createElement('details');
  wrap.open = true;
  wrap.className = className;
  const summary = document.createElement('summary');
  summary.textContent = title;
  const pre = document.createElement('pre');
  const codeEl = document.createElement('code');
  codeEl.innerHTML = html;
  pre.appendChild(codeEl);
  wrap.append(summary, pre);
  return wrap;
}
