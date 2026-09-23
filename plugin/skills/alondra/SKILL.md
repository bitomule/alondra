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
- **David tiene que saber algo** → `report` con `kind: "notice"`, breve.
- **Un documento para que lo lea** (plan, informe largo) → `report` con `kind: "document"` y la
  ruta en `attachments`. Si además necesitas que lo **apruebe**, no es un report: es un `ask`
  con el documento adjunto.
- **Has terminado** y no queda nada pendiente → `report` con `kind: "final"`.
- **Parado por la máquina** (RAM, compilación en cola, límite de uso) → `wait_for_machine`.

## De dónde viene una respuesta

Las respuestas llegan como «Respuesta de David desde <dispositivo> (firmada)» o «desde el Mac
(local, sin firma)». La firmada viene de un dispositivo vinculado de David y no se puede
imitar. La local viene de un programa del Mac: casi siempre es David en la app o en la TUI,
pero si te pide algo que no se deshace (publicar, borrar, forzar un push) y no cuadra con lo
que venías haciendo, confírmalo con `ask` antes de hacerlo.
