# G2 PR y diff: integración de H

`AlondraPullRequestsFeature` se compila aparte de iOS y Mac. H es el único dueño
de `apple/BUILD.bazel`, las raíces y las vistas actuales. Al conectar el módulo:

1. Añadir `//apple/Features/PullRequests:AlondraPullRequestsFeature` a los dos
   targets de app. Crear un `StoreOf<PullRequestFeature>` por tarjeta o destino y
   un `StoreOf<DiffReviewFeature>` por revisión. La selección y los borradores
   pertenecen a la escena/ventana; la sesión y la cola de comandos son del proceso.
   Las vistas leen `state` y envían acciones. Retirar sus `@State` de red,
   validación, merge, comentarios, colapso y envío; no llamar a `FleetStore` ni a
   `AlondraClient` desde `PRReviewView`, `PRActions`, `PullRequestDetail` o
   `PRDiffReview`.
2. Alimentar `.pullRequestChanged` desde el snapshot **autoritativo** del PR
   (incluidos checks, mergeability, métodos y estado) mientras el destino exista.
   `nil` solo si el servidor ya no incluye ese PR; no enviar `nil` por ocultación
   local mientras una mutación está pendiente. En desconexión enviar
   `.snapshotUnavailable`, y en cambio de identidad `.identityChanged` a ambos
   stores para invalidar tareas, diff y borradores. El refresco de checks cambia
   `state.pr` sin duplicar tarjetas.
3. Instalar `PullRequestOperations` en la composición. `diff` usa el cliente
   seleccionado por la misma identidad de la sesión. Las cinco operaciones de
   mutación pasan por el `CommandGate` durable, que reserva el objetivo antes de
   subir adjuntos y evita dos efectos simultáneos o reintentos tras resultado
   `.unknown(id)`. En el contrato backend #156, SHA `86b4c33`, HTTP 409
   `pr_not_open` se mapea a
   `.closed` (mantiene borrador y muestra el cierre), demás rechazos a
   `.rejected(mensaje)`, cambio
   de credencial a `.obsolete`. Reconciliar solo con `.commandReconciled(id)` del
   journal. Para `ask`, la tarjeta permanece en inbox; para `requestChanges`,
   merge y conflictos, retirar solo cuando se confirme el servidor.
   `shouldDismiss` es una señal de presentación, no prueba de estado GitHub.
   El adaptador traduce una respuesta 502 posterior a un POST a
   `PRPostDispatchUncertain`; pasar `PRPostFailureClassifier.isUnknown` a
   `CommandGate.submit`. El resultado es `.unknown(journalID)`, aunque el body
   diga `response_too_large`: el Mac podría haber ejecutado el comando. Nunca
   reintentarlo automáticamente ni tratarlo como rechazo.
4. `DiffReviewFeature` contiene comentarios por ancla, nota, adjuntos, línea en
   edición y archivos colapsados. iOS presenta `LineCommentSheet` y Mac un editor
   inline, ambos con `.editLine`, `.saveLineComment` y `.cancelLineComment`.
   `PreparePRReviewUseCase` ordena comentarios como `ReviewExport`, rechaza anclas
   que ya no están en el diff y exige reconfirmación explícita si el contenido
   del diff cambia aunque `path:line` siga igual. H muestra
   `commentsNeedReview` y envía `.confirmCommentsForCurrentDiff` solo cuando la
   persona ha visto la nueva revisión. Las anclas que desaparezcan se muestran
   como comentarios pendientes y pueden quitarse con `.removeLineComment`.
   El borrador se conserva en caso de error. Antes de enviar **cualquier**
   review, el reducer vuelve a leer el diff por la misma identidad y exige
   igualdad exacta con lo mostrado. Si la lectura falla o difiere, no reserva
   ni envía la mutación; la persona revisa la nueva versión o reintenta la
   carga. Una nota general también espera esa comprobación. El snapshot conserva además
   `headSHA` por escena: el preflight exige el mismo SHA aunque el texto no haya
   cambiado y el POST envía ese SHA como `expected_head_sha`. El backend #165
   comprueba el head antes de entregar la review. Conservar las vistas de
   checks, descripciones, preguntas y reportes existentes como presentación
   hasta que G1 se conecte; sus datos vienen del snapshot, sin repetir entradas.
5. El adaptador del servicio usa el contrato backend #165, SHA `a7b1e9c`:
   `GET /v1/prs/{id}/diff-snapshot` devuelve `diff`, `head_sha`, `truncated`.
   Un snapshot truncado se rechaza antes de crear anclas. Una respuesta local GET mayor de 180 KiB devuelve sobre cifrado
   `status:502` con `response_too_large`; mapear **solo ese código** a
   `PRDiffLoadError.responseTooLarge`. Otros fallos GET devuelven 502
   `read_failed`; no convertirlos en «diff demasiado grande» ni en diff vacío.
   El 502 de respuesta a POST se clasifica como resultado incierto después del
   dispatch incluso si usa `response_too_large`, porque el side effect ya puede
   haberse producido.
   `ask`/`requestChanges`/`resolveConflicts` devuelven 409 `pr_not_open` si el
   daemon ya sabe que el PR está cerrado. Si GitHub lo cierra antes de que el
   daemon lo observe, queda una carrera: el cliente no debe prometer que su
   comprobación local impide una mutación tardía. Los uploads binarios requieren otro
   protocolo cifrado: con relay activo, el adaptador de `review` debe rechazar
   adjuntos con mensaje explícito antes de reservar o enviar; no inventar wire.

Tras H: compilar app iOS y Mac, y comprobar con MAV apertura de PR, checks que
cambian mientras el diálogo está abierto, comentario en línea, 409 de PR cerrado,
rechazo/resultado incierto sin doble envío y dos ventanas Mac con borradores
independientes. El módulo solo comprueba decisiones; no acredita por sí mismo UI
ni transporte E2E.

Bloqueo de release pendiente: uploads cifrados de adjuntos. La composición rechaza
reviews con adjuntos antes del CommandGate y conserva el borrador; esto no acredita
paridad funcional ni permite dar la migración por terminada.
