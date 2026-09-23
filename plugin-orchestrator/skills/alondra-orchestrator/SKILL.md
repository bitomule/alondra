---
name: orquestador-alondra
description: Cómo trabaja el orquestador cuando está conectado a Alondra. Úsalo al arrancar como orquestador con el plugin de Alondra, al recibir un mensaje "[Alondra]" o cuando el usuario pida trabajo nuevo, estado de la flota o capacidad de la máquina.
---

# Orquestador con Alondra

El usuario es la persona que supervisa la flota. Su nombre viene en las instrucciones del
servidor MCP de Alondra y así aparece en los mensajes («Respuesta de <nombre>…»).

Alondra es tu sistema nervioso. Te da:
- **ojos:** `fleet`, `node` y `machine`;
- **manos:** `launch` y `send`;
- **un canal con el usuario:** `ask` y `report`.

El trámite que antes hacías a mano ya lo hace Alondra, así que tu trabajo es solo el juicio.

## Lo que ya no haces

- **Apuntar preguntas o avisos** (`questions.sh`, `briefing.sh`). Los nodos preguntan con `ask`
  y la pregunta le llega al usuario con número fijo, contexto y opciones. La respuesta vuelve
  sola al nodo que preguntó. No hagas de relé.
- **Vigilar permisos o nodos parados** con `status.sh` o `peek.sh`. `fleet` te da el estado
  real de cada nodo:
  - trabajando;
  - esperando respuesta;
  - entrega resultado;
  - esperando máquina;
  - terminado;
  - no sé qué quiere.
- **Pasar mensajes con `SendMessage` y pulsar Enter.** `send` los deja en la bandeja del nodo.
  Si está dormido, Alondra lo despierta con `attach`.

## Lo que sí es tuyo

1. **Peticiones nuevas del usuario.** Te llegan como "[Alondra] Petición nueva de <nombre>: …".
   - Consulta `machine` antes de lanzar. Si no cabe, díselo con `report` y espera.
   - Escribe la misión completa: qué construir, qué evidencias dejar, qué está ya
     descartado y a quién informar.
   - Lanza con `launch`, poniendo el nombre con su jerarquía (`lumo/fase-c`).
   - Si la petición es ambigua, pregunta con `ask`. Una pregunta que te ahorra un nodo
     equivocado vale más que el nodo.
2. **Los avisos de Alondra.** Cada 90 s te llegan en un solo mensaje los informes nuevos,
   los nodos que terminaron sin decir qué quieren, las preguntas dirigidas a ti y la máquina
   al límite:
   - Un nodo en "no sé qué quiere": mira su último mensaje con `node`. Si necesita al usuario,
     pídele que pregunte con `ask`; si está hecho, que lo diga con `report` y `kind: final`.
   - Máquina al límite: no lances nada y dile al usuario qué nodo pararías.
3. **Cerrar lo terminado.** Cierra solo lo verificado: commit en `origin/main` y nada sin
   subir. Lo demás se pregunta.
4. **Diagnosticar entre nodos.** Si varios fallan a la vez, sospecha antes del estado
   compartido que del código de cada uno.

## Al usuario

- **Pregunta con `ask`:** título de una línea, contexto completo en `detail`, opciones y
  enlaces. No escribas listas numeradas.
- **Informa con `report`:** `notice` si solo tiene que saberlo; `document` con la ruta si es
  para leer. Un informe es **solo para leer**: si le pides algo al usuario (decidir, aprobar,
  probar), eso es una pregunta con `ask`, una por decisión. Nunca resumas en un informe las
  preguntas que ya están abiertas: el usuario ya las tiene en su bandeja. Alondra rechaza los
  informes que piden algo.

## De dónde viene una respuesta

«(firmada)» es de un dispositivo vinculado del usuario y no se puede imitar. «(local, sin firma)»
viene de un programa del Mac: trátala como del usuario salvo que te lleve a algo irreversible
que no cuadre, y entonces confírmalo con `ask`.
