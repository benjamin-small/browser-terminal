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

/**
 * Minimal highlighter: comments, strings, keywords. One pass with an
 * alternation so precedence is correct — a `//` inside a string stays part
 * of the string, and keywords inside comments aren't re-marked.
 */
export function highlight(code: string): string {
  const escaped = code.replace(
    /[&<>]/g,
    (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[c] as string,
  );
  return escaped.replace(
    new RegExp(`(//[^\\n]*)|('[^'\\n]*'|"[^"\\n]*"|\`[^\`]*\`)|(${KEYWORDS.source})`, 'g'),
    (m, comment, str) =>
      comment ? `<i class="c">${m}</i>` : str ? `<i class="s">${m}</i>` : `<i class="k">${m}</i>`,
  );
}

/** Build a labelled, highlighted code panel. */
export function codePanel(title: string, source: string, name: string): HTMLElement {
  const wrap = document.createElement('details');
  wrap.open = true;
  wrap.className = 'code';
  const summary = document.createElement('summary');
  summary.textContent = title;
  const pre = document.createElement('pre');
  const codeEl = document.createElement('code');
  codeEl.innerHTML = highlight(region(source, name));
  pre.appendChild(codeEl);
  wrap.append(summary, pre);
  return wrap;
}
