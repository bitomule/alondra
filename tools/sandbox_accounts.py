#!/usr/bin/env python3
"""Connect Claude on a sandbox Mac before its first request can run."""
import argparse
import json
import urllib.request


def connect_claude(port):
    def settings(method, path, body=None):
        request = urllib.request.Request(
            f"http://127.0.0.1:{port}/settings/accounts{path}?capabilities=provider_accounts",
            method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            return json.load(response)

    listing = settings("GET", "")
    config = listing["config"]
    connected = config.get("connected") or []
    config["connected"] = connected if "claude" in connected else [*connected, "claude"]
    settings("PUT", "", {"version": listing["version"], "accounts": config})
    settings("POST", "/check")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("port", type=int)
    args = parser.parse_args()
    try:
        connect_claude(args.port)
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"Could not connect Claude on sandbox port {args.port}: {error}\n")


if __name__ == "__main__":
    main()
