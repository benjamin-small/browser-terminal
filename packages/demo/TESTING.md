# Manual test checklist

Run `just demo`, open http://localhost:5173, then walk through:

## Shell basics
- [ ] Banner + green `❯` prompt appear in the floating panel
- [ ] `help` lists commands as a table; `help where` shows usage
- [ ] `echo a b c | str upcase` renders an indexed list
- [ ] `links --limit 20 | where text ne '' | head 5` renders a box table
- [ ] `sort-by`, `get`, `to json --pretty`, `from json` behave
- [ ] `links | grep 'rust|xterm' -i` filters by real regex (alternation works)
- [ ] `links | grep '^https' --column href` restricts to one column; `-v` inverts
- [ ] `links | grep '('` shows a clean "invalid regex pattern" error, engine survives
- [ ] Bad input: `sort-by n --reverze` → red caret + "did you mean `--reverse`?"
- [ ] Unknown command `nop 5` → caret + help; `str upcsae` suggests `str upcase`
- [ ] Prompt turns red after a failure, green after success

## Line editor
- [ ] Arrows/Home/End/C-a/C-e move; backspace/delete/C-u/C-k/C-w edit
- [ ] ↑/↓ walk history; the in-progress line is stashed and restored
- [ ] Paste multi-line text → inserted as one line with spaces, never auto-runs
- [ ] C-l clears; C-c cancels the line

## Async + cancellation
- [ ] `slow 15` ticks progressively; typing still echoes during it
- [ ] Ctrl-C prints `^C`, stops the ticks, prompt returns
- [ ] `fail` shows the rich error with help line
- [ ] Console: `bt.run("links | length")` resolves a number

## Multiplexer
- [ ] Ctrl-B % / Ctrl-B " split; focus follows the new pane (blue outline)
- [ ] Ctrl-B o / arrows move focus; click a pane to focus it
- [ ] Ctrl-B z zooms and unzooms
- [ ] Ctrl-B x kills a pane; layout collapses; last pane recreates a fresh one
- [ ] Ctrl-B c new window; tabs update; Ctrl-B n/p cycle; tab click switches
- [ ] `slow 30` in pane A; typing in pane B stays responsive
- [ ] Dragging a divider resizes panes live
- [ ] PREFIX badge lights up while the prefix is armed

## Sessions + panel
- [ ] `session new work` forks; dock pills appear; pill click switches
- [ ] Scrollback survives switching away and back
- [ ] Drag panel by header; resize by right/bottom edges and corner
- [ ] Minimize (─) → pills only; pill click restores
- [ ] Ctrl-B d hides the panel; Ctrl+` (globalToggle) brings it back
- [ ] `bt.dispose()` in console removes everything; no stray keys/listeners
