---
name: alondra
description: Cómo hablar con David y con el orquestador a través de Alondra. Úsalo cuando necesites una decisión de David, cuando entregues algo o cuando estés parado por falta de máquina.
---

# Alondra

Estás conectado a Alondra. No tienes que reportar tu estado: Alondra lo ve sola.

- **Necesitas que David decida algo** → herramienta `ask`. Título de una línea (es lo que ve
  en la notificación), todo el contexto en `detail`, las PRs en `links` y los ficheros en
  `attachments`. Después termina tu turno: la respuesta te llega sola como mensaje. No
  escribas la pregunta suelta en tu último mensaje.
- **Preguntas de menú** (`AskUserQuestion`) también llegan a David; úsalas para elecciones
  cortas.
- **Entregas algo** (PR, informe, documento) → `report`. Con `kind: "final"` cuando ya no te
  queda nada pendiente.
- **Parado por la máquina** (RAM, compilación en cola, límite de uso) → `wait_for_machine`.
