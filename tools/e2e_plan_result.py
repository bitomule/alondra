#!/usr/bin/env python3
"""A research that ends in a plan, and one that ends in a report alone, end to end against a sandbox.

A node without a model writes its report in a temporary folder, as a background session writes
in $CLAUDE_JOB_DIR/tmp, and delivers a final result that cites that file and proposes a plan.
The folder is deleted at once: the daemon must already keep the whole result in its files folder.
That result is a plan card: the plan in front, the report as the document «Informe» beside it,
and only «Aprobar» and «Pedir cambios». The «Solo archivar» button an app may still show from
before is refused and closes nothing. A second node delivers a report alone: its card offers
«Visto», and seeing it creates nothing.

`--client tui` opens both cards in the real TUI inside tmux and approves the plan with its first
button; `--client phone` approves it from the sandbox phone. Either way the request that holds
the plan exists afterwards, and none for the report. Captures land in --captures.
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

NEED = "Aprueba el plan para indexar toda la galería de Redraft"
SEEN = "Lee el informe sobre limpiar los duplicados de Redraft"


def deliver(node, interaction, need, body, plan=None):
    result = {"task_id": node.task, "interaction_id": interaction, "title": need, "need": need,
              "content": {"kind": "report", "body": body},
              "targets": [{"kind": "text", "text": "Informe"}], "is_final": True}
    if plan:
        result["plan"] = plan
    node.call(("present_result", result))


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


def request_exists(phone, request):
    try:
        return phone.view(f"request:{request}")
    except AssertionError:
        return None


def request_created(phone, request):
    deadline = time.time() + e2e_cards.TIMEOUT
    while time.time() < deadline:
        if view := request_exists(phone, request):
            return view
        time.sleep(0.5)
    return None


def press(phone, epoch, action_id, outcome="applied"):
    reply = phone.request("POST", "/actions", {"protocol": 1, "command_id": str(uuid.uuid4()), "data_epoch": epoch,
                                               "client": "ios", "lang": "es", "action_id": action_id})
    assert reply["outcome"] == outcome, reply
    return reply


def select(tui, need):
    for key in ["k"] * 8 + ["j"] * 8:
        if any(line.startswith("│▌") and need[:30] in line for line in tui.screen().splitlines()):
            return
        tui.keys(key)
    raise AssertionError(f"{need!r} was never selected")


def approve_in_tui(sandbox, captures):
    tui = e2e_cards.Terminal([sandbox.alondra], sandbox.env, captures)
    try:
        tui.until(lambda s: "TE ESPERA" in s, "the inbox never showed")
        select(tui, SEEN)
        tui.until(lambda s: "Visto" in s, "the report alone never offered «Visto»")
        tui.capture("report-alone")
        select(tui, NEED)
        tui.until(lambda s: "Aprobar" in s and "Solo archivar" not in s and "Aceptar" not in s and "Informe" in s,
                  "the plan card never offered «Aprobar» with its report beside it")
        tui.capture("plan-card")
        tui.keys("f")
        tui.until(lambda s: "elige" in s, "the documents of the card never opened")
        tui.keys("1")
        tui.until(lambda s: "Viable: indexar una vez" in s, "the report never opened")
        tui.capture("plan-card-report")
        tui.keys("Escape")
        tui.until(lambda s: e2e_cards.listed(s, NEED), "back to the inbox")
        select(tui, NEED)
        tui.keys("Enter")
        tui.until(lambda s: "Guardar la huella de cada foto" in s, "the card never showed its plan")
        tui.capture("plan-card-detail")
        tui.keys("Escape")
        tui.until(lambda s: e2e_cards.listed(s, NEED), "back to the inbox")
        select(tui, NEED)
        tui.keys("1")
        tui.until(lambda s: not e2e_cards.listed(s, NEED), "the card was still listed once approved", seconds=5)
        tui.capture("plan-card-approved")
        print("ok (tui): the report offers «Visto», the plan card shows its plan with the report beside it, and approving it takes it out",
              file=sys.stderr, flush=True)
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
        deliver(nodes[1], "duplicates", SEEN, "Los duplicados se limpian bien con lo que hay: no hace falta construir nada.")
        shutil.rmtree(job_tmp)
        text, path = kept(sandbox, "research")
        assert "Seguir los cambios de la fototeca" in text and "- [ ] Mostrar el progreso del índice" in text, text
        print(f"ok: the report outlived its temporary folder, kept whole in {path}", file=sys.stderr, flush=True)

        phone = e2e_cards.Phone(sandbox)
        card, actions, epoch = card_of(phone, NEED)
        labels = [action["label"] for action in actions]
        assert labels[:1] == ["Aprobar"] and "Solo archivar" not in labels and "Aceptar" not in labels, labels
        view = json.dumps(phone.view(f"card:{card}"), ensure_ascii=False)
        assert "Informe" in view and "Guardado en" in view and "Guardar la huella de cada foto" in view, view
        with open(os.path.join(args.captures, "card-plan-result-es.json"), "w") as file:
            file.write(view)
        print(f"ok: the card shows the plan with its report beside it, and offers {labels}", file=sys.stderr, flush=True)

        approve = next(a for a in actions if a["label"] == "Aprobar")["id"]
        old_archive = approve.rsplit("~", 1)[0] + "~archive"
        press(phone, epoch, old_archive, outcome="no_longer_available")
        card_of(phone, NEED)
        print("ok: an old «Solo archivar» button is refused and the plan card stays", file=sys.stderr, flush=True)

        seen, seen_actions, seen_epoch = card_of(phone, SEEN)
        seen_labels = [action["label"] for action in seen_actions]
        assert seen_labels[:1] == ["Visto"] and "Aprobar" not in seen_labels, seen_labels
        if args.client == "tui":
            approve_in_tui(sandbox, args.captures)
        else:
            press(phone, epoch, approve)
        press(phone, seen_epoch, next(a for a in phone.actions(seen)[0] if a["label"] == "Visto")["id"])
        approved = request_created(phone, "implement-research")
        assert approved and "Guardar la huella de cada foto" in json.dumps(approved, ensure_ascii=False), approved
        assert request_exists(phone, "implement-duplicates") is None, "seeing a report created a request"
        print("ok: approving created the request that holds the plan; seeing the report created nothing", file=sys.stderr, flush=True)
        print(f"e2e plan result ({args.client}): a plan is built only when approved, a report is only seen, and the report is never lost.")
    finally:
        for node in nodes:
            node.stop()
        sandbox.stop()


if __name__ == "__main__":
    main()
