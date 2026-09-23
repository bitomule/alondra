**Objetivo:** sí, me lo diste — tier 2 (308,33 €/mes). Está puesto y he quitado el "tier 3" que seguía pidiéndote; no se vuelve a preguntar.

**Para qué es la clave:** leer el índice de popularidad de búsqueda de Apple (5-100), la única fuente fiable de cuánta gente busca cada término. Sin ella ningún run puede proponer cambios de keywords con números. No se crea ninguna campaña ni hay gasto.

**El par de claves ya está generado** en `~/.asc/`. Tu parte son solo clics.

### Pasos

1. **Entra en https://ads.apple.com** con tu Apple ID de desarrollador.
   Si nunca has tenido cuenta de Apple Ads, créala y **elige Advanced, no Basic** (la API solo existe en Advanced). Puede pedirte datos de facturación. **No crees ninguna campaña.**

2. **Invítate con un rol de API.** Tu nombre (arriba a la derecha) → **Account Settings → User Management → Invite Users**.
   - Correo: uno tuyo; sirve un alias, p. ej. `nombre+ads@example.com`.
   - Rol: **API Account Read Only** (basta para leer popularidad).
   - ⚠️ Obligatorio: si entras como administrador, el campo para pegar la clave **no aparece**.

3. **Acepta la invitación** desde ese correo y **entra en ads.apple.com como ese usuario nuevo**.

4. **Account Settings → pestaña API.** Pega esta clave pública en *Public Key* y guarda:

```
-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEexampleexampleexampleexample
exampleexampleexampleexampleexampleexample==
-----END PUBLIC KEY-----
```

5. **Contéstame aquí con los tres valores** que aparecen:
   - **clientId** (empieza por `SEARCHADS…`)
   - **teamId** (también `SEARCHADS…`, pero distinto)
   - **keyId**

Con eso lo registro en `asc`, compruebo que funciona leyendo la popularidad de un término, y borro el fichero de la clave privada (queda en el llavero).

---
Aviso de uso: hoy va el 28% del presupuesto semanal contra el 18% que le toca a un día. No hago nada más en este nodo hasta tu respuesta o hasta que mergees alguno de los 5 PR.
