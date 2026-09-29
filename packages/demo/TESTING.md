# Manual test checklist

Run `just demo`, open http://localhost:5173, then walk through:

## Shell basics
- [ ] Banner + green `❯` prompt appear in the floating panel
- [ ] `help` lists commands as a table; `help where` shows usage
- [ ] `echo a b c | str upcase` renders an indexed list
- [ ] `links --limit 20 | filter {|o| $o.text != ''} | head 5` renders a box table
- [ ] `sort-by`, `get`, `to json --pretty`, `from json` behave
- [ ] `links | grep 'rust|xterm' -i` filters by real regex (alternation works)
- [ ] `links | grep '^https' --on href` restricts to one field; `-v` inverts
- [ ] `links | map '(o) => o.text'` projects; `map '(o) => ({a: o.text})'` reshapes
- [ ] `links | filter '(o) => o.text.length > 4'` filters by predicate
- [ ] `links | map @host` uses the registered function (no eval needed)
- [ ] `links | map @hostt` suggests `@host`; `length --on x` rejects the flag
- [ ] `links | sort-by --on '(o) => o.text.length'` sorts by a computed key
- [ ] `links | filter {|o| $o.text.length > 4}` — native closure, no JS engine
- [ ] `links | map {|o| $o.text + ' -> ' + $o.href}` concatenates
- [ ] `links | filter {|o| $o.missing > 5}` returns 0 rows, does NOT error
- [ ] `echo 1 | map {|o| $o.a` reports an unterminated closure
- [ ] The same closure lines work in `cargo run -p bterm-cli` (no JS there)
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
- [ ] Console: `bt.run("links | length")` resolves `{ value, log, err }`, with `value` the number

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

## Filesystem and editing (record browser/version)
- [ ] `ca` then Tab completes `cat`; `cat welcome.txt` displays its contents
- [ ] `cat ` then Tab lists entries; filename prefixes and quoted paths complete
- [ ] `cd ` then Tab suggests directories; `ls --lo` then Tab completes `--long`
- [ ] Typing while a completion is pending preserves the newer input
- [ ] Tab completes registered and multiword commands after pipes; ambiguous names list candidates
- [ ] Prompt shows cwd after `cd`, session switches, pane splits, and unmount
- [ ] Startup uses `/scratch`; `ls`, editing, saving, and redirects need no permissions
- [ ] On HTTPS/localhost in Chrome and Edge, Connect local folder requests read access and mounts even when write permission is missing
- [ ] Cancelling the picker leaves mounts and cwd unchanged
- [ ] Pending selection disables duplicate attempts; cancellation and browser errors show a useful status and re-enable connection
- [ ] A rejected selection reports the original error; AbortError is not presented as proof of cancellation
- [ ] Select a disposable fixture folder; `ls`, nested `cd`, and `cat` read its actual files
- [ ] A selected folder appears at `/mnt/<folder-name>`; duplicate names use `-2`, `-3`, etc.
- [ ] `ls /mnt` lists connected folders; Tab completes their paths
- [ ] `cd ..` from its mount root reaches virtual `/mnt`, then `/`, never the real parent folder
- [ ] Editing and Save request no implicit write grant; Enable writes appears only if permission is missing and prompts directly
- [ ] Grant write permission, save, and verify the file bytes with a native editor
- [ ] Denied/revoked permission leaves editor text available for export
- [ ] Modify a file externally; Save reports a conflict and retains the unsaved buffer
- [ ] LF/CRLF and UTF-8 BOM survive save; mixed endings are read-only
- [ ] Close/Reload dirty text offers a discard decision; Ctrl/Cmd-S saves and focus returns on close
- [ ] Edit welcome.txt, reload page, and verify automatic `/scratch` startup and saved contents
- [ ] Browser files returns to `/scratch` after visiting a local mount
- [ ] On Firefox and Safari, verify unavailable picker messaging and test scratch capabilities
- [ ] `read-bytes /dev/zero --length 16` works; unbounded reads and device redirects fail
- [ ] Unmount/dispose retains dirty text for export and rejects later file writes
