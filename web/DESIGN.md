---
version: alpha
name: Alondra public site
description: getalondra.com. Explains how Alondra turns a request into parallel agent tasks and brings only the decisions to the user, and takes installs and Alondra Cloud waitlist sign-ups.
colors:
  primary: "#DF6E1B"
  on-primary: "#131B25"
  accent-ink: "#A84100"
  focus: "#A84100"
  surface: "#F9F5EC"
  surface-2: "#F2ECE1"
  surface-3: "#E8E0D3"
  card: "#FEFCF7"
  rule: "#D3CDC2"
  on-surface: "#1C252E"
  on-surface-2: "#3B434D"
  on-surface-3: "#5D646C"
  ok: "#287C42"
  error: "#BA3535"
  stage: "#0E1720"
  stage-2: "#19232D"
  stage-3: "#252F3A"
  stage-rule: "#353E48"
  on-stage: "#F8EFDD"
  on-stage-2: "#CAC3B6"
  on-stage-3: "#8F9AA4"
  stage-accent: "#F79133"
  terminal: "#091318"
  on-terminal: "#D8DFE3"
  on-terminal-dim: "#80878B"
  dark-surface: "#091018"
  dark-surface-2: "#0F1821"
  dark-card: "#131D27"
  dark-rule: "#2A343E"
  dark-on-surface: "#F7EEDB"
  dark-on-surface-2: "#CDC7B9"
  dark-primary: "#F79133"
  dark-accent-ink: "#FC9E47"
  dark-stage: "#03080E"
typography:
  headline-display: { fontFamily: Archivo, fontSize: 89.6px, fontWeight: 600, lineHeight: 1.02, letterSpacing: -0.03em, fontVariation: "'wdth' 112" }
  headline-hero: { fontFamily: Archivo, fontSize: 62.4px, fontWeight: 700, lineHeight: 1.0, letterSpacing: -0.035em, fontVariation: "'wdth' 112" }
  headline-lg: { fontFamily: Archivo, fontSize: 54.4px, fontWeight: 600, lineHeight: 1.08, letterSpacing: -0.02em, fontVariation: "'wdth' 108" }
  headline-md: { fontFamily: Archivo, fontSize: 36.8px, fontWeight: 600, lineHeight: 1.08, letterSpacing: -0.02em }
  headline-sm: { fontFamily: Archivo, fontSize: 25.6px, fontWeight: 600, lineHeight: 1.08, letterSpacing: -0.02em }
  body-lg: { fontFamily: Archivo, fontSize: 25.6px, fontWeight: 400, lineHeight: 1.3 }
  body-md: { fontFamily: Archivo, fontSize: 19px, fontWeight: 400, lineHeight: 1.6 }
  body-sm: { fontFamily: Archivo, fontSize: 15px, fontWeight: 400, lineHeight: 1.6 }
  label-md: { fontFamily: Archivo, fontSize: 15px, fontWeight: 600, lineHeight: 1.3 }
  label-sm: { fontFamily: Archivo, fontSize: 13.4px, fontWeight: 600, lineHeight: 1.5 }
  code: { fontFamily: Martian Mono, fontSize: 15px, fontWeight: 400, lineHeight: 1.7 }
rounded:
  none: 0px
  sm: 7.2px
  md: 12.8px
  lg: 20px
  phone: 38.4px
  full: 9999px
spacing:
  3xs: 4px
  2xs: 8px
  xs: 12px
  sm: 16px
  md: 24px
  lg: 32px
  xl: 48px
  2xl: 64px
  3xl: 96px
  4xl: 128px
  container: 1344px
components:
  button-primary: { backgroundColor: "{colors.primary}", textColor: "{colors.on-primary}", typography: "{typography.label-md}", rounded: "{rounded.sm}", height: 48px }
  button-primary-on-stage: { backgroundColor: "{colors.stage-accent}", textColor: "{colors.on-primary}", typography: "{typography.label-md}", rounded: "{rounded.sm}", height: 48px }
  text-link: { textColor: "{colors.accent-ink}", typography: "{typography.label-md}" }
  capture: { backgroundColor: "{colors.surface-2}", rounded: "{rounded.md}" }
  command: { backgroundColor: "{colors.terminal}", textColor: "{colors.on-terminal}", typography: "{typography.code}", rounded: "{rounded.sm}", padding: 24px }
  soon-badge: { textColor: "{colors.accent-ink}", typography: "{typography.label-sm}", rounded: "{rounded.full}" }
  waitlist-input: { backgroundColor: "{colors.card}", textColor: "{colors.on-surface}", rounded: "{rounded.sm}", height: 48px }
---

## Overview

The site is a working notebook about one product. The hero sits on a dark stage with the three real
apps stacked as evidence (Mac diff, terminal app, iPhone card), all captured by `make demo-captures`
from an invented demo world. Below the hero the page turns to warm paper and reads like a document:
numbered steps, one replay, one diagram, rows of facts. Visitors are developers who already run
Claude Code or Codex; they should leave knowing what Alondra does differently and either install it
or join the Alondra Cloud waitlist.

Signature moves: the real captures do the selling, never mock-ups; the architecture diagram is
drawn in the page's own tokens with the direct path in the accent colour; unfinished work carries a
"Coming soon" badge instead of being left out silently or sold as done.

The CSS mirrors these tokens as `oklch()` custom properties in `web/app/public/tokens.css`; the hex
values here are their sRGB fallbacks. Dark mode follows the system (`prefers-color-scheme`) and
overrides the same names; components never branch on the mode.

## Colors

- `primary` (orange): primary buttons, the direct-connection line in the diagram, the waitlist
  button. Never body text on paper: use `accent-ink` for links and badges there.
- `accent-ink`: text links, step numbers, "Coming soon" badges, the focus ring on paper.
- `surface` / `surface-2` / `surface-3`: page, footer and diagram groups, inline code. Neutrals are
  tinted warm (hue 80 to 85, chroma 0.012 to 0.02).
- `on-surface`, `on-surface-2`, `on-surface-3`: headings, body copy, captions. All pass 4.5:1 on
  `surface`.
- `stage` family: the hero, the header on the home page and the Alondra Cloud band. Text on it uses
  the `on-stage` ramp, tinted from the stage, never grey.
- `terminal` family: the command blocks and the terminal replay.
- `ok` and `error`: the waitlist's success mark and error border only.
- The `dark-*` entries document the dark-mode values of the same roles.

## Typography

One family for prose and one for code: Archivo (variable) at width 108 to 112 for headings and at
its normal width for body copy, labels and buttons; Martian Mono only inside commands and the
replay. Body copy is 19px at weight 400 with a 1.6 line height, because the thin serif it replaced
read poorly at 18px, above all in dark mode. Both are self-hosted (`web/app/public/fonts`) with `font-display: swap`. Display sizes are fluid
(`clamp()`); the values in the tokens are their maximum. Headings use `text-wrap: balance`.

## Layout

One container of 84rem with a fluid gutter (`clamp(1rem, 4vw, 3.5rem)`). Sections are separated by
a 1px rule that starts and ends with the content, and get 96px of padding on desktop, 64px under
62rem. Text measure stays at 60 to 66ch. Breakpoints: 62rem (navigation collapses into a menu,
grids go to one column) and 40rem (the diagram switches to its vertical version, the actions take
the full width).

## Elevation & Depth

Only captures carry a shadow, because they stand for windows and phones. Everything else separates
with a tone step (`surface-2`, `card`) or a rule.

## Shapes

`sm` for buttons, inputs and code blocks; `md` for captures and the replay; `lg` for the phone in
the card journey; `phone` for a phone capture shown on its own; `full` for the "Coming soon" badge.

## Motion

Four pieces move, all played once when they scroll into view (`useArmed`): the flow diagram
lights the request, the plan, the working tasks, the waiting task and the cards in turn, the terminal replay
types a request and resolves three tasks, the card journey highlights the same card in the terminal
and on the iPhone, and the diagram sends sealed envelopes along the direct and relayed paths.
Easing `cubic-bezier(0.16, 1, 0.3, 1)`, durations 120ms (controls), 220ms and 420ms. Under
`prefers-reduced-motion: reduce`, or without scripting, every piece shows its final state and
nothing moves.

## Do's and Don'ts

- Do show the product through the real captures in `web/app/public/captures` (regenerated by
  `make demo-captures`), in light and dark.
- Do mark anything not on main as "Coming soon", and keep the claims table in the PR up to date.
- Do keep `primary` for actions and the direct path; one orange thing per view is the goal.
- Don't add gradients, glows, stock illustrations, emoji or decorative icons.
- Don't add testimonials, logos, counters or performance numbers that nobody measured.
- Don't put shadows on cards or rows.
