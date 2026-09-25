# H1 iOS triaje: alcance y H2

H1 conecta una instancia de `TriageFeature` por escena al `IOSAppSync` del proceso. `IOSAppSync.project(_:)` publica el overview que recibe del único `SyncSession` en un stream de proyección de triaje; no abre otra suscripción de red. `TriageFeature` posee cola, cursor, detalle, skip y ocultación optimista. `markRead` y `setAside` entran por `TriageService.mutate` al `FleetStore.submitCommand` que comparte el `CommandGate` durable. El cambio de servidor/credencial invalida sincrónicamente cada store antes de sustituir el cliente. Un push para pregunta o informe agrupado enfoca la tarjeta de su PR.

La ruta push conserva la solicitud si el ítem no está en el snapshot actual y espera una proyección posterior al push. Observa la revisión del snapshot y los destinos agrupados de PR. Un skip animado lleva la identidad capturada al reducer para que un gesto antiguo no mueva una tarjeta con el mismo ID tras rotar credenciales.

## Trabajo concreto de H2 / G1 / G2

- `TriageCard.swift:58-60`: el handoff de los detalles legacy vuelve a `FleetStore.resolve`. Cuando G1/G2 tengan reducer, el detalle debe enviar un resultado tipado al store de triaje y dejar de transferir cierres de cliente.
- `TriageCard.swift:504,551,644-678,701-731`: respuesta de pregunta, petición de contexto, respuesta/revisión de informe y acciones de PR aún llaman `AlondraClient` o `FleetStore` desde la vista. Llevar formularios y estado de envío a G1/G2; use cases para negocio; servicios para I/O; `CommandGate` solo en el adaptador app. La ocultación optimista posterior a esas acciones debe actualizar el store de triaje, no esperar al siguiente overview.
- `TriageCard.swift:591`: `AttachmentStrip` aún recibe `fleet.client`. G1/G2 debe exponer un servicio estrecho para adjuntos y contemplar el límite del sobre E2E y el protocolo de chunks pendiente antes de habilitar upload iOS.
- `TriageCard.swift:103,164-166` y `TriageView.swift:357-445`: datos de nodos, tareas y máquina siguen en `FleetStore`. Mover la proyección de solo lectura al estado apropiado de los features cuando F3/H lo permitan.
- `TriageView.swift:82-93`: la ruta push aún se consume en la vista y el aviso "ya no pendiente" se escribe en `FleetStore`. H2 debe mover la ruta y ese aviso al root/feature y enlazar el detalle G1/G2 al ID del hijo agrupado.
- `TriageView.swift:44-53`: `ReviewView` y `PRReviewView` siguen siendo detalles legacy. Preservan la navegación visual hasta G1/G2.
- `TriageView.swift:178-273`: el `CardStack` iOS existente consume estado/acciones de F1 para conservar la altura y animación actuales; el `TriageDeck` de `AlondraTriagePresentation` no se usa aún. H2 puede unificar ambos adaptadores después de trasladar esas transiciones sin regresión visual.
- `AlondraApp.swift:130-164`: toast global y advertencias inciertas todavía proceden de `FleetStore`; el estado de triaje muestra su propio aviso de rechazo/resultado desconocido.
- Pairing/settings, fuera de H1: el comando de incorporación al Mac es `alondra setup`.

H1 no supone paridad iOS sobre el relay: el backend exige pairing Mac aprobado, token OIDC, firma del dispositivo y sobres E2E. El transporte local legacy de `IOSAppSync` será sustituido por H de RelayClient cuando el wire de pairing esté congelado.
