#!/usr/bin/env python3
"""Reads a document with Mermaid diagrams in the real TUI against the e2e sandbox, at 80 and 120 columns.

Usage: tools/e2e-doc-mermaid.py <alondrad> <alondra> <alondra-relay> <alondra-sandbox>

The sandbox's scripted node writes a document with a flowchart, a sequence, a state and a class
diagram, a left-to-right flowchart, a broken diagram and a pie chart, and asks for its review, as
tools/e2e-doc-review.py does. `alondra` runs in tmux. At 80 columns the four kinds draw, the
left-to-right flowchart is redrawn top-down, and the broken diagram and the pie show their code with
a notice. At 120 columns the comments panel opens beside the document, which leaves it no wider, so
the same diagrams draw and the left-to-right one is still redrawn top-down. E2E_CAPTURES=<dir> keeps
what the TUI showed at each step there; the sandbox and every other file it made are removed at the
end, whatever happens.
"""

import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_spec = importlib.util.spec_from_file_location("e2e_doc_review", os.path.join(REPO, "tools/e2e-doc-review.py"))
review = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(review)

DOCUMENT = """# Diagrams

## Flowchart

```mermaid
flowchart TD
  A[Card arrives] --> B{Needs user?}
  B -->|yes| C[Ask David]
  B -->|no| D[Run node]
  C --> E[Deliver result]
  D --> E
```

## Sequence

```mermaid
sequenceDiagram
  participant TUI
  participant Daemon
  participant Node
  TUI->>Daemon: open task
  Daemon->>Node: start session
  Node-->>Daemon: result ready
  Daemon-->>TUI: show card
```

## State

```mermaid
stateDiagram-v2
  [*] --> Queued
  Queued --> Working: capacity
  Working --> Waiting: asks
  Waiting --> Working: answered
  Working --> [*]
```

## Class

```mermaid
classDiagram
  class Task {
    +String id
    +close()
  }
  class Plan {
    +Vec~Step~ steps
  }
  Task "1" --> "1" Plan
```

## Left to right

```mermaid
flowchart LR
  R[Request] --> P{Plan needed?}
  P -->|yes| W[Write plan]
  P -->|no| S[Start node]
  W --> D[Done]
  S --> D
```

## Broken

```mermaid
flowchart TD
  A -->
  B[[[ -->
```

## Pie

```mermaid
pie title Where the time goes
  "Reviews" : 5
  "Builds" : 3
```
"""


def jump(tui, text):
    tui.send("/")
    tui.type(text)
    tui.send("Enter")


def main():
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    binaries = [os.path.abspath(p) for p in sys.argv[1:] if not p.endswith(".ok")]
    daemon, cli, relay, tool = binaries
    scratch = os.path.realpath(tempfile.mkdtemp(prefix="alondra-mermaid-", dir="/private/tmp"))
    sandbox = os.path.join(scratch, "sandbox")
    tui = None
    try:
        env = dict(os.environ, SANDBOX_DIR=sandbox, SANDBOX_PORT=os.environ.get("SANDBOX_PORT", "47950"), MACS="1", FAKES="1")
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox.sh"), daemon, cli, relay, tool], env=env, check=True, stdout=subprocess.DEVNULL)
        home = json.load(open(os.path.join(sandbox, "sandbox.json")))["macs"][0]["home"]
        child = {k: v for k, v in os.environ.items() if not k.startswith("ALONDRA_")}
        for line in open(os.path.join(home, "env")):
            key, _, value = line.removeprefix("export ").strip().partition("=")
            child[key] = value.strip('"')
        child.update(TERM="xterm-256color", ALONDRA_LANG="es", ALONDRA_SANDBOX="1")
        node = review.Node(sandbox, child)
        node.call(node.document("diagrams-r1", DOCUMENT, first=True), *node.ask("diagrams-1", "diagrams-r1", "Review the diagrams"))
        captures = os.environ.get("E2E_CAPTURES") or os.path.join(scratch, "captures")
        os.makedirs(captures, exist_ok=True)
        tui = review.Terminal([os.path.join(sandbox, "bin/alondra")], child, captures)
        tui.tmux("resize-window", "-t", "0", "-x", "80", "-y", "50")
        tui.expect("Review the diagrams")
        tui.send("Enter")
        tui.expect("< Needs user? >")
        tui.capture("80-flowchart")

        jump(tui, "Sequence")
        tui.expect("│─open task────────>│")
        tui.capture("80-sequence")
        jump(tui, "State")
        tui.expect("│ Queued │")
        tui.capture("80-state")
        jump(tui, "Class")
        tui.expect("│ +String id │")
        tui.capture("80-class")
        print("ok: the four kinds draw at 80 columns", file=sys.stderr, flush=True)

        jump(tui, "Left to right")
        screen = tui.expect("< Plan needed? >")
        assert "►< Plan needed? >" not in screen, f"the left-to-right flowchart kept its direction at 80 columns:\n{screen}"
        tui.capture("80-left-to-right")
        print("ok: a left-to-right flowchart too wide for 80 columns is redrawn top-down", file=sys.stderr, flush=True)

        jump(tui, "Broken")
        screen = tui.expect("El diagrama tiene un error en la línea 2; se muestra su código")
        assert "B[[[ -->" in screen, screen
        tui.capture("80-broken")
        jump(tui, "Pie")
        screen = tui.expect("Este tipo de diagrama aún no se dibuja aquí")
        assert "pie title Where the time goes" in screen, screen
        tui.capture("80-pie")
        print("ok: a broken diagram and a pie show their code with a notice", file=sys.stderr, flush=True)

        tui.tmux("resize-window", "-t", "0", "-x", "120", "-y", "50")
        jump(tui, "Flowchart")
        tui.expect("< Needs user? >")
        tui.capture("120-flowchart")
        tui.expect("Comentarios 0")
        jump(tui, "Sequence")
        tui.expect("│─open task────────>│")
        tui.capture("120-sequence")
        jump(tui, "Left to right")
        screen = tui.expect("< Plan needed? >")
        assert "►< Plan needed? >" not in screen, f"the left-to-right flowchart overflowed beside the comments panel:\n{screen}"
        tui.capture("120-left-to-right")
        jump(tui, "Broken")
        tui.expect("El diagrama tiene un error en la línea 2; se muestra su código")
        tui.capture("120-broken")
        print("ok: at 120 columns, beside the comments panel, the diagrams still draw within the document", file=sys.stderr, flush=True)
        if os.environ.get("E2E_CAPTURES"):
            print(f"TUI captures in {captures}", file=sys.stderr, flush=True)
        print("e2e doc mermaid: the TUI drew the diagrams that fit and showed the code of the rest with a notice.")
    except BaseException:
        if tui:
            print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        raise
    finally:
        if tui:
            tui.stop()
        print("stopping the sandbox", file=sys.stderr, flush=True)
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox-stop.sh"), sandbox], stdout=subprocess.DEVNULL, timeout=120)
        shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    main()
