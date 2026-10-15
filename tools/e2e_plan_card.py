#!/usr/bin/env python3
"""A plan that cites a Markdown file reaches the user with the file inside, against its own sandbox.

A node without a model posts two plans through the same `alondra node hold` its mod runs: one that
only points at a file its worktree does not have, which goes back to the node at once, and one that
cites `docs/fast-connection-plan.md` from its worktree, which the daemon holds with the file's
content at its end. Then each client shows that card: `--client tui` in the real TUI inside tmux,
`--client ios` in the iOS app in a pooled simulator (E2E_IOS_IPA, as make e2e-ios-cards), and
`--client mac` saves the Mac view the daemon serves for it, which the Mac snapshot tests draw.
Captures land in --captures.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_cards  # noqa: E402

FILE = """# Plan de conexión rápida

## Decisión

iroh con relay propio: una ida y vuelta por petición, y conexión directa cuando la red lo permite.

## Pasos

- [ ] 0. Medir el estado actual en la app
- [ ] 1. Relay de iroh de producción con control de acceso
- [ ] 2. Daemon con conexión iroh

## Medidas

| Ruta | Tiempo |
| --- | --- |
| Buzón actual | 375 ms |
| Relay propio | 63–94 ms |
| Directa | 55 ms |
"""

PLAN = """# Paso 1: relay de iroh de producción

## Contexto

Paso 1 del plan de conexión rápida, en docs/fast-connection-plan.md.

## Checklist

- [ ] 1. Relay de iroh con comprobación de acceso
- [ ] 2. Registro de los identificadores de dispositivos y Macs
"""

NAME = f"Plan card {os.getpid()}"

POINTER = "# Paso 1\n\nEl plan completo está en docs/missing-plan.md.\n\n- [ ] Hacerlo\n"


def hold(node, card_id, plan):
    card = {"session_id": node.session, "cwd": node.workdir, "id": card_id, "kind": "plan", "input": {"plan": plan}}
    process = subprocess.Popen([node.alondra, "node", "hold"], env=node.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True)
    process.stdin.write(json.dumps(card))
    process.stdin.close()
    node.holds.append(process)
    return process


def plan_card(phone, client):
    """The held plan's card id and the view the daemon serves for it to `client`."""
    deadline = time.time() + e2e_cards.TIMEOUT
    while time.time() < deadline:
        queue, _ = phone.queue()
        for card in queue:
            view = phone.request("GET", f"/views/card:{card}?protocol=1&client={client}&lang=es")
            if "docs/fast-connection-plan.md" in json.dumps(view, ensure_ascii=False) and "Relay propio" in json.dumps(view, ensure_ascii=False):
                return card, view
        time.sleep(0.5)
    raise AssertionError("the plan with its cited file never reached the queue")


def check_tui(sandbox, captures):
    tui = e2e_cards.Terminal([sandbox.alondra], sandbox.env, captures)
    try:
        tui.until(lambda s: "TE ESPERA" in s, "the inbox never showed")
        for _ in range(8):
            if any(line.startswith("│▌") and "Paso 1" in line for line in tui.screen().splitlines()):
                break
            tui.keys("j")
        tui.keys("Enter")
        tui.until(lambda s: "Paso 1" in s, "the plan never opened")
        tui.capture("plan")
        for _ in range(6):
            if "Relay propio" in tui.screen():
                break
            tui.keys("PageDown")
        tui.until(lambda s: "docs/fast-connection-plan.md" in s and "Relay propio" in s, "the included file never showed")
        tui.capture("plan-included-file")
        print("ok (tui): the plan shows the cited file's content", file=sys.stderr, flush=True)
    except BaseException:
        print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        raise
    finally:
        tui.stop()


def pair(simulator, sandbox):
    """Pairs with this sandbox's Mac, found by its name among other sandboxes on Bonjour."""
    import re
    simulator.until(lambda ids: "findMac.allow" in ids or "findMac.mac" in ids, "the app never showed pairing", 60)
    if "findMac.allow" in simulator.ids():
        simulator.tap("findMac.allow")
    deadline, line = time.time() + 60, None
    while line is None:
        assert time.time() < deadline, f"{NAME} never appeared on Bonjour: " + " | ".join(
            l for l in simulator.mav("ui", "tree").splitlines() if "findMac" in l)
        line = next((l for l in simulator.mav("ui", "tree").splitlines() if " id=findMac.mac " in l and NAME in l), None)
        time.sleep(0.5)
    x, y, width, height = map(float, re.findall(r"[-\d.]+", line.split('frame="')[1].split('"')[0]))
    simulator.mav("ui", "tap", "--x", str(int(x + width / 2)), "--y", str(int(y + height / 2)))
    sandbox.approve_pairing()
    simulator.until(lambda ids: any(i.startswith("triage.need.") for i in ids), "the queue never showed", 60)


def check_ios(sandbox, card, captures):
    import e2e_ios_cards
    import tempfile
    ipa = os.environ.get("E2E_IOS_IPA")
    assert ipa and os.path.exists(ipa), "E2E_IOS_IPA names the simulator build of //apple:AlondraiOS with --define=alondra_sandbox=1"
    scratch = tempfile.mkdtemp(prefix="alondra-ios-plan-")
    simulator = e2e_ios_cards.Simulator(ipa, scratch)
    try:
        pair(simulator, sandbox)
        simulator.tap(f"triage.need.{card}")
        simulator.until(lambda ids: "cardDetail.loaded" in ids, "the plan's screen stayed loading", e2e_ios_cards.DETAIL)
        time.sleep(e2e_ios_cards.SHOWN)
        simulator.capture(captures, "plan")
        for page in (1, 2, 3):
            simulator.mav("ui", "swipe", "--start-x", "200", "--start-y", "600", "--end-x", "200", "--end-y", "300")
            time.sleep(e2e_ios_cards.SHOWN)
            simulator.capture(captures, f"plan-scrolled-{page}")
        print("ok (ios): the plan shows the cited file's content", file=sys.stderr, flush=True)
    except BaseException:
        simulator.capture(captures, "failure")
        print(simulator.mav("ui", "tree", check=False), file=sys.stderr, flush=True)
        raise
    finally:
        simulator.stop()
        shutil.rmtree(scratch, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--client", choices=["tui", "ios", "mac"], required=True)
    parser.add_argument("--captures", required=True)
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    os.makedirs(args.captures, exist_ok=True)
    os.environ["SANDBOX_NAME"] = NAME
    if args.client == "ios":
        os.environ["SANDBOX_LAN"] = "1"
    sandbox = e2e_cards.Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "47950")))
    node = None
    try:
        node = e2e_cards.Node(sandbox)
        os.makedirs(os.path.join(node.workdir, "docs"))
        with open(os.path.join(node.workdir, "docs/fast-connection-plan.md"), "w") as file:
            file.write(FILE)
        pointer = hold(node, "toolu-pointer", POINTER)
        reply = json.loads(pointer.communicate(timeout=e2e_cards.TIMEOUT)[0])
        assert reply.get("approved") is False and "cannot open docs/missing-plan.md" in reply.get("feedback", ""), reply
        print(f"ok: a plan that only points at a file goes back to its node: {reply['feedback']}", file=sys.stderr, flush=True)
        hold(node, "toolu-plan", PLAN)
        phone = e2e_cards.Phone(sandbox, direct=True)
        card, view = plan_card(phone, "mac" if args.client == "mac" else "ios")
        print(f"ok: the plan reached the queue with the file inside ({card})", file=sys.stderr, flush=True)
        if args.client == "mac":
            with open(os.path.join(args.captures, "card-plan-cited-file-es-mac.json"), "w") as file:
                json.dump(view, file, ensure_ascii=False, indent=2)
                file.write("\n")
        elif args.client == "tui":
            check_tui(sandbox, args.captures)
        else:
            check_ios(sandbox, card, args.captures)
    finally:
        if node:
            node.stop()
        sandbox.stop()


if __name__ == "__main__":
    main()
