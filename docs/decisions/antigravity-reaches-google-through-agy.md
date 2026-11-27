+++
id = "antigravity-reaches-google-through-agy"
title = "Alondra reaches Google only through agy, and through Google's public API to check a Gemini key"
status = "accepted"
date = "2026-11-27"
supersedes = []
superseded_by = ""
+++

## Context

Antigravity supports external clients only through ACP. Calling Antigravity's internal endpoints can
get a Google account flagged. Alondra still needs to know whether an account is signed in, which
models it offers, how much quota is left, and whether a Gemini API key is valid: real `agy` accepts
any key when asked to authenticate.

## Decision

Everything Alondra learns from Google about an Antigravity account comes from the `agy` binary:
`agy --version`, `agy acp` (initialize, authenticate, sessions, session options) and
`agy -p /usage`. Alondra never calls an internal Google endpoint. The one exception, decided by the
user on 2026-11-26, is the check of a Gemini API key, made against Google's public Gemini API when
the key is saved and in the daily account check. Adding any other direct call to Google needs the
user's approval.

## Consequences

Facts about `agy` that were never measured stay assumptions, and the fake `agy` and the conformance
transcripts mark them so. Quota and usage are only as fresh as the last `/usage` reading.

## Confirmation

`antigravity.boundary/reaches-google-only-through-agy`, `antigravity.account-check`.
