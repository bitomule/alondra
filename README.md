# Alondra

El sistema nervioso del orquestador de la colmena: estado fiable de cada nodo de Claude Code
y de la máquina, preguntas estructuradas que llegan a David y vuelven a su nodo, y
herramientas para que el orquestador actúe.

| Carpeta | Qué es |
|---|---|
| `crates/core` | Modelo y reductor de estados (Rust, sin E/S) |
| `crates/alondrad` | Demonio 24/7: hooks, vigilante, SQLite, API |
| `crates/alondra` | CLI, TUI, servidor MCP de los nodos y hook de bandeja |
| `plugin/` | Plugin de Claude Code: hooks, herramientas `ask`/`report` y skill |
| `launchd/` | Servicio de macOS para `alondrad` |
| `docs/plan.md` | Diseño y plan por fases |

## Probarlo

```bash
make install        # compila con Bazel e instala alondra y alondrad en ~/.local/bin
make launchd        # alondrad arranca solo al iniciar sesión y se reinicia si cae
alondra             # la TUI
alondra status      # lo mismo en texto
```

Un nodo conectado a Alondra se lanza con el plugin:

```bash
claude --bg --name "prueba/alondra" --plugin-dir ~/Projects/alondra/plugin --permission-mode auto -- "<misión>"
```

o, desde el orquestador, con su herramienta `launch`.

**El orquestador** se arranca con `--plugin-dir ~/Projects/alondra/plugin-orchestrator`: los
mismos hooks más las herramientas `fleet`, `node`, `machine`, `send` y `launch`. Alondra le
avisa sola, cada 90 s y en un único mensaje, de:
- los informes nuevos de los nodos;
- las preguntas dirigidas a él;
- los nodos que terminan sin decir qué quieren;
- la máquina, cuando se queda sin memoria o se satura.

(`claude --bg` no hereda variables de entorno, así que el rol va en su propio plugin.)

### La TUI

- **Te esperan**: preguntas de los nodos (estructuradas, de menú, permisos y planes).
  `Enter` las abre con todo su contexto, `1`–`4` elige opción y `r` responde con texto.
- **Flota**: todo lo que lista `claude agents`, con su estado (trabajando, esperando respuesta,
  entrega resultado, esperando máquina, terminado, no sé qué quiere). `~` = lo dice el demonio
  de Claude Code, `*` = lo ha decidido jev, `💤` = dormido (sin proceso; se despierta con
  `attach` al mandarle un mensaje).
- `m` manda un mensaje al nodo seleccionado y `o`, al orquestador.

## Desarrollo

`make build` / `make test` con Bazel. `cargo build` / `cargo test` también funcionan, para
iterar rápido y para rust-analyzer.
