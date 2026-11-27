+++
id = "antigravity-blocked-tools-hold-the-account"
title = "An Antigravity account whose agy does not run Alondra's tools holds its task and takes no more nodes"
status = "accepted"
date = "2026-11-27"
supersedes = []
superseded_by = ""
+++

## Context

A node delivers through Alondra's MCP tools. An organisation's admin controls can keep `agy` from
starting a local MCP server that a session names. The node then cannot deliver, and every turn ends
without a result.

## Decision

The user decided on 2026-11-27: when a node's `agy` does not start Alondra's tools, the task stops on
a card that says this Antigravity account does not let nodes use Alondra's tools. It is not retried
by itself, and the account takes no more nodes until the user fixes it and tries again.

## Consequences

The host must be able to tell, from a typed signal, that the tools are missing.

## Confirmation

Not built yet. The change that builds it adds its scenario to `antigravity.nodes`: blocked tools
hold the task and the account.
