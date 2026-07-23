<script lang="ts">
  import { store } from './tasks.svelte';
  import { highlight, region } from './code';
  // Real source, extracted at build time — the example can't drift.
  import commandsSource from './commands.ts?raw';
  import storeSource from './tasks.svelte.ts?raw';

  const panels = [
    ['Registering commands over a rune store', commandsSource, 'commands'],
    ['The store itself (module-level $state)', storeSource, 'store'],
  ] as const;

  // Derived from the same signal the shell commands mutate.
  const remaining = $derived(store.tasks.filter((t) => !t.done).length);
</script>

<main>
  <h1>browser-terminal <span class="x">×</span> Svelte</h1>
  <p class="lede">
    These commands read and write a module-level <code>$state</code> rune — registered
    once at startup, with no hooks, refs, or cleanup.
  </p>

  <ul class="tasks">
    {#each store.tasks as task (task.id)}
      <li class:done={task.done}>
        <span class="id">#{task.id}</span>
        <span class="title">{task.title}</span>
        <span class="pri p{task.priority}">P{task.priority}</span>
      </li>
    {:else}
      <li class="empty">no tasks — try `task add 'something'`</li>
    {/each}
  </ul>
  <p class="count">{remaining} remaining / {store.tasks.length} total</p>

  <h2>Try</h2>
  <pre>{`task add 'ship it' --priority 3
tasks | filter {|t| !$t.done}
tasks | sort-by priority --reverse | head 2
tasks | map @slug
task done 2`}</pre>
  <h2>How it's implemented</h2>
  <p class="sub">Extracted from this page's own source at build time — not a transcription.</p>
  {#each panels as [title, source, name] (name)}
    <details class="code" open>
      <summary>{title}</summary>
      <pre><code>{@html highlight(region(source, name))}</code></pre>
    </details>
  {/each}

  <p class="note">
    Compare with the React demo on :5174 — there, commands must reach state through a
    latest-ref hook, because <code>useState</code> is scoped to a render. A Svelte rune
    lives outside the component, so the shell can just hold it.
  </p>
</main>

<style>
  :global(:root) {
    color-scheme: dark;
  }
  :global(body) {
    margin: 0;
    background: #11111b;
    color: #cdd6f4;
    font: 15px/1.6 system-ui, sans-serif;
  }
  main {
    max-width: 680px;
    margin: 3rem auto;
    padding: 0 1.25rem 420px;
  }
  h1 {
    font-size: 1.5rem;
    margin-bottom: 0.25rem;
  }
  .x {
    color: #9399b2;
  }
  h2 {
    font-size: 1rem;
    margin-top: 2rem;
    color: #9399b2;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .lede {
    color: #9399b2;
    margin-top: 0;
  }
  code,
  pre {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
  }
  pre {
    background: #181825;
    border: 1px solid #313244;
    border-radius: 8px;
    padding: 0.85rem 1rem;
    overflow-x: auto;
    color: #a6e3a1;
  }
  .tasks {
    list-style: none;
    padding: 0;
    margin: 1.5rem 0 0.5rem;
    border: 1px solid #313244;
    border-radius: 8px;
    overflow: hidden;
  }
  .tasks li {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.6rem 0.9rem;
    border-bottom: 1px solid #313244;
  }
  .tasks li:last-child {
    border-bottom: none;
  }
  .tasks li.done .title {
    text-decoration: line-through;
    color: #9399b2;
  }
  .tasks li.empty {
    color: #9399b2;
    font-style: italic;
  }
  .id {
    color: #9399b2;
    font-family: ui-monospace, monospace;
    font-size: 12px;
    min-width: 2.2rem;
  }
  .title {
    flex: 1;
  }
  .pri {
    font-family: ui-monospace, monospace;
    font-size: 11px;
    padding: 1px 7px;
    border-radius: 999px;
    border: 1px solid #313244;
    color: #9399b2;
  }
  .pri.p3 {
    color: #f38ba8;
    border-color: #f38ba8;
  }
  .pri.p2 {
    color: #f9e2af;
    border-color: #f9e2af;
  }
  .count {
    color: #9399b2;
    font-size: 13px;
    margin: 0;
  }
  .note {
    color: #9399b2;
    font-size: 13px;
  }
  .sub {
    color: #6c7086;
    font-size: 13px;
    margin-top: -0.4rem;
  }
  details.code {
    border: 1px solid #313244;
    border-radius: 8px;
    background: #181825;
    margin: 0.75rem 0;
    overflow: hidden;
  }
  details.code > summary {
    cursor: pointer;
    padding: 0.6rem 0.9rem;
    font-size: 0.85rem;
    user-select: none;
  }
  details.code > summary:hover { background: #1e1e2e; }
  details.code pre {
    margin: 0;
    padding: 0.9rem;
    border-top: 1px solid #313244;
    border-radius: 0;
    color: #cdd6f4;
  }
  details.code code { font-size: 12.5px; line-height: 1.55; white-space: pre; }
  details.code :global(.c) { color: #6c7086; font-style: normal; }
  details.code :global(.s) { color: #a6e3a1; font-style: normal; }
  details.code :global(.k) { color: #cba6f7; font-style: normal; }
</style>
