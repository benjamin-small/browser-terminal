# Language direction: structured values and host integration

Status: adopted. This records the direction decision in issue #11 and applies
to future language changes. Historical design documents remain useful records
of earlier implementations; their POSIX comparisons and reserved-syntax plans
do not establish compatibility requirements.

browser-terminal is an embedded structured shell. Its central contract is typed
values flowing between commands authored by the shell or its host application.
POSIX shell compliance, script portability, and operating-system process or
file-descriptor semantics are not goals.

## Rules for language changes

- Preserve value types across pipes and host boundaries. Strings, records,
  lists, and bytes do not become display text merely because they are piped.
  Rendering happens at the terminal boundary; explicit commands such as
  `to json` perform serialization.
- Let the host define application resources. A target may name a document,
  a virtual filesystem entry, or another application object. The core does
  not infer filesystem access, a current directory, or an encoding from it.
- Keep diagnostics separate from values. Progress and warnings do not become
  pipeline input; errors determine whether execution succeeds.
- Adopt familiar syntax when it improves the structured model. Spelling a
  feature like a shell operator does not import the corresponding POSIX rules.
  Missing POSIX features are not automatically roadmap items.
- Preserve existing behavior unless an issue explicitly changes it. This
  decision is not a reason to remove useful quoting, flags, pipes, closures,
  or other established syntax.

## Redirection is a concrete application of this direction

Issue #13 adds optional host hooks for `<`, `>`, and `>>`. A read provides a
value to a pipeline. A write receives its collected value; append is intent
that the host interprets. Neither operation implies opening a file, converting
values to bytes, selecting a file descriptor, or redirecting diagnostics.

The host must install a handler to enable the grammar. Target expressions
resolve to strings, but the payload stays typed, including binary data. A
failed pipeline does not call its output writer. The public package README
documents the precise syntax, collection rules, and cancellation contract.
