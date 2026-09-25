# Alondra icon

`Alondra.icon` is the editable Icon Composer document used by both app targets.
Its Default appearance
uses a native System Light background and `d-modular-lark-v3-light-foreground.png`
with a dark body. Its Dark appearance uses the native dark counterpart and
`d-modular-lark-v3-safe-foreground.png` with an ivory body. Appearance-specific
layer opacity switches between the two artworks. Liquid Glass, translucency
and shadow effects are disabled. The bird fits inside the icon mask with clear
margins. The background is configured in Icon Composer, not supplied as an image.

Six transparent foreground concepts generated for review on 25 September 2026. They are source explorations; the installed app icon is `Alondra.icon`.

| File | Idea | Suggested layers after selection |
| --- | --- | --- |
| `a-lark-baton-foreground.png` | A lark emerging from a baton stroke | background / orange bird / baton tip |
| `b-conductor-gesture-foreground.png` | A baton coordinating three arcs | background / arcs / baton |
| `c-winged-monogram-foreground.png` | An abstract `a` with wings | background / wings / letter stroke |
| `d-modular-lark-foreground.png` | A bird assembled from cooperating parts | background / paired wings / central body and cue |
| `d-modular-lark-v2-foreground.png` | David's selected direction, without the orange line or endpoints inside the body | background / paired wings / central body |
| `d-modular-lark-v2-wings.png` | The wings and hinges extracted from the selected direction | wing layer |
| `d-modular-lark-v2-body.png` | The ivory head, body and tail extracted from the selected direction | body layer |
| `e-orchestra-lark-foreground.png` | Orchestra marks that suggest a bird | background / ensemble arcs and points / cue |
| `f-abstract-cue-foreground.png` | A wing gesture, three agents and a cue | background / three wing strokes / dots / baton |

All source artwork is 1254 × 1254 PNG with alpha. The selected direction has a complete transparent foreground and separate wing/body explorations. The independently generated layers are approximate, so align and clean their edges before a production export at 1024 × 1024. Generated edge artifacts and tiny details should not be treated as final production artwork. Preview at 32 and 64 px before integration.

Apple's [app icon guidance](https://developer.apple.com/design/human-interface-guidelines/app-icons) recommends a separate background and crisp foreground layers, and [Icon Composer](https://developer.apple.com/documentation/xcode/creating-your-app-icon-using-icon-composer) can set material effects and appearance variants.

Generation method: built-in Codex image generation, one prompt per concept. Prompts specified a transparent, centered, minimal logo-brand foreground using the lark, conductor, agent and orchestra ideas; no text, background, squircle or watermark.
