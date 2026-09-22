# Alondra

El sistema nervioso del orquestador de la colmena: estado fiable de cada nodo de Claude Code
y de la máquina, preguntas estructuradas que llegan a David y vuelven a su nodo, y
herramientas para que el orquestador actúe.

Piezas, todas en este repo y todas con Bazel:

| Carpeta | Qué es |
|---|---|
| `crates/core` | Modelo y reductor de estados (Rust, sin E/S) |
| `crates/alondrad` | Demonio 24/7: ingesta de hooks, vigilante, SQLite, API |
| `crates/alondra` | CLI y TUI |
| `plugin/` | Plugin de Claude Code: hooks y herramientas para los nodos |
| `apple/` | App de iOS y macOS (SwiftUI) |

Diseño y plan: `docs/plan.md`.
