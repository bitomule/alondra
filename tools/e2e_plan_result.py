#!/usr/bin/env python3
"""A research result that proposes a plan, end to end against its own sandbox.

A node without a model writes its report in a temporary folder, as a background session writes
in $CLAUDE_JOB_DIR/tmp, and delivers a final result that cites that file and proposes a plan.
The folder is deleted at once: the daemon must already keep the whole result in its files folder,
and the card must show the report, the cited file, the plan and where it is kept, with «Aprobar y
empezar a implementar» and «Solo archivar» instead of «Aceptar».

`--client tui` opens that card in the real TUI inside tmux and approves it with its first button;
`--client phone` approves it from the sandbox phone. Either way the orchestrator is then told the
request that holds the plan. A second node's result is only archived: the orchestrator hears
nothing about it. Captures land in --captures.
"""

import argparse
import glob
import json
import os
import shutil
import sys
import tempfile
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_cards  # noqa: E402

REPORT = """# Redraft: índice de toda la galería

## Fases

1. Guardar la huella de cada foto en Core Data.
2. Seguir los cambios de la fototeca en segundo plano.
"""

PLAN = """# Índice de toda la galería

- [ ] Guardar la huella de cada foto en Core Data, junto a lo que los usuarios ya tienen
- [ ] Seguir los cambios de la fototeca en segundo plano
- [ ] Mostrar el progreso del índice en la pantalla de inicio
"""

NEED = "Lee el informe sobre indexar toda la galería de Redraft"
ARCHIVED = "Lee el informe sobre limpiar los duplicados de Redraft"


def deliver(node, interaction, need, body, plan):
    node.call(("present_result", {"task_id": node.task, "interaction_id": interaction, "need": need,
                                  "content": {"kind": "report", "body": body}, "plan": plan,
                                  "targets": [{"kind": "text", "text": "Informe"}], "is_final": True}))


def kept(sandbox, interaction):
    found = glob.glob(os.path.join(sandbox.env["ALONDRA_HOME"], "files/results/*", f"{interaction}.md"))
    assert len(found) == 1, f"the result {interaction} is kept once: {found}"
    return open(found[0]).read(), found[0]


def card_of(phone, need):
    deadline = time.time() + e2e_cards.TIMEOUT
    while time.time() < deadline:
        for card in phone.queue()[0]:
            actions, epoch, shown = phone.actions(card)
            if shown == need:
                return card, actions, epoch
        time.sleep(0.5)
    raise AssertionError(f"{need!r} never reached the queue")


def orchestrator_heard(sandbox, text):
    deadline = time.time() + e2e_cards.TIMEOUT
    while time.time() < deadline:
        if os.path.exists(sandbox.orchestrator_log) and text in open(sandbox.orchestrator_log).read():
            return True
        time.sleep(0.5)
    return False


def press(phone, epoch, action):
    reply = phone.request("POST", "/actions", {"protocol": 1, "command_id": str(uuid.uuid4()), "data_epoch": epoch,
                                               "client": "ios", "lang": "es", "action_id": action["id"]})
    assert reply["outcome"] == "applied", reply


def approve_in_tui(sandbox, captures):
    tui = e2e_cards.Terminal([sandbox.alondra], sandbox.env, captures)
    try:
        tui.until(lambda s: "TE ESPERA" in s, "the inbox never showed")
        for _ in range(8):
            if any(line.startswith("│▌") and NEED[:30] in line for line in tui.screen().splitlines()):
                break
            tui.keys("j")
        tui.until(lambda s: "Aprobar y empezar a implementar" in s and "Solo archivar" in s, "the card never offered its two choices")
        tui.capture("plan-result")
        tui.keys("Enter")
        tui.until(lambda s: "Plan propuesto" in s or "Plan propuesto" in s.replace("\n", " "), "the card never opened")
        for _ in range(8):
            if "Guardado en" in tui.screen():
                break
            tui.keys("PageDown")
        tui.until(lambda s: "Guardado en" in s, "the card never said where the result is kept")
        tui.capture("plan-result-detail")
        tui.keys("Escape")
        tui.until(lambda s: "Aprobar y empezar a implementar" in s, "back to the inbox")
        tui.keys("1")
        tui.until(lambda s: not e2e_cards.listed(s, NEED), "the card was still listed once approved", seconds=5)
        tui.capture("plan-result-approved")
        print("ok (tui): the card shows its plan and where it is kept, and approving it takes it out", file=sys.stderr, flush=True)
    except BaseException:
        print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        raise
    finally:
        tui.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--client", choices=["tui", "phone"], required=True)
    parser.add_argument("--captures", required=True)
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    os.makedirs(args.captures, exist_ok=True)
    sandbox = e2e_cards.Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "47950")))
    nodes = []
    try:
        nodes = [e2e_cards.Node(sandbox), e2e_cards.Node(sandbox)]
        job_tmp = tempfile.mkdtemp(prefix="job-tmp-", dir=sandbox.scratch)
        report = os.path.join(job_tmp, "redraft-indice-galeria.md")
        with open(report, "w") as file:
            file.write(REPORT)
        deliver(nodes[0], "research", NEED, f"Viable: indexar una vez y seguir los cambios. Informe completo en {report}", PLAN)
        deliver(nodes[1], "duplicates", ARCHIVED, "Los duplicados se limpian bien con lo que hay.", PLAN)
        shutil.rmtree(job_tmp)
        text, path = kept(sandbox, "research")
        assert "Seguir los cambios de la fototeca" in text and "- [ ] Mostrar el progreso del índice" in text, text
        print(f"ok: the report outlived its temporary folder, kept whole in {path}", file=sys.stderr, flush=True)

        phone = e2e_cards.Phone(sandbox, direct=True)
        card, actions, epoch = card_of(phone, NEED)
        labels = [action["label"] for action in actions]
        assert "Aprobar y empezar a implementar" in labels and "Solo archivar" in labels and "Aceptar" not in labels, labels
        view = json.dumps(phone.view(f"card:{card}"), ensure_ascii=False)
        assert "Plan propuesto" in view and "Guardado en" in view and "Guardar la huella de cada foto" in view, view
        with open(os.path.join(args.captures, "card-plan-result-es.json"), "w") as file:
            file.write(view)
        print(f"ok: the card shows the plan and where it is kept, and offers {labels}", file=sys.stderr, flush=True)

        archived, _, archive_epoch = card_of(phone, ARCHIVED)
        press(phone, archive_epoch, next(a for a in phone.actions(archived)[0] if a["label"] == "Solo archivar"))
        if args.client == "tui":
            approve_in_tui(sandbox, args.captures)
        else:
            press(phone, epoch, next(a for a in actions if a["label"] == "Aprobar y empezar a implementar"))
        assert orchestrator_heard(sandbox, "Request implement-research holds that plan"), open(sandbox.orchestrator_log).read()
        assert "implement-duplicates" not in open(sandbox.orchestrator_log).read(), "an archived plan reached the orchestrator"
        print("ok: approving told the orchestrator the request that holds the plan; archiving told it nothing", file=sys.stderr, flush=True)
        print(f"e2e plan result ({args.client}): the plan is built only when approved and the report is never lost.")
    finally:
        for node in nodes:
            node.stop()
        sandbox.stop()


if __name__ == "__main__":
    main()
