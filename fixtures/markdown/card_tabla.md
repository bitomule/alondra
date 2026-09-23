**Nada de instalar: dos cosas que copiar, el resto ya lo tenemos mejor.** Repo leído entero (40 skills, sin scripts salvo un validador; no pide credenciales nuestras, no ejecuta nada).

## Tabla

| Lo que traen | ¿Lo tenemos? | ¿Cambia lo que haríamos? |
|---|---|---|
| **Fuente de datos = Appeeky** (API/MCP de pago, 8 $/mes) para volumen, dificultad, rankings, estimaciones de descargas | No, y a propósito | **No.** Sin esa clave sus skills no tienen dato: recomiendan de memoria. Su volumen/dificultad son estimaciones de un proveedor; nuestro `suggest.js` lee el autocompletado real de la tienda (lo medido en `reference_aso_keyword_verification_autocomplete`) y `store-signals.sh` lee las keywords que Apple indexa hoy por locale. No hay dato mejor delante, así que no se toca |
| Fórmula de oportunidad `Volumen×0,4 + (100−Dificultad)×0,3 + Relevancia×0,3` | Parecido (ordenamos por relevancia/tráfico/competencia sin número) | **No.** Dos de los tres términos son estimaciones de Appeeky y el tercero (relevancia) se lo inventa el modelo. Un número inventado ordena peor que un juicio explícito |
| **`in-app-events`** — ficha de evento en la pestaña Hoy y en búsqueda | **No tenemos nada** | **Sí.** Es superficie gratis que ni tocamos: 6 tipos, 30/50/120 caracteres, imagen 2160×1080, hasta 10 vivos. Para Lumo o Quill un evento tipo *Major Update* en cada release pone la app en Hoy sin pagar nada |

## La suma

**2 filas de 40 cambian algo**: In-App Events y Custom Product Pages. Ambas son superficies de la tienda, no métodos — cosas que Apple ofrece y nosotros no usamos. El método suyo es peor que el nuestro en todo lo que se solapa, porque su capa de datos es un SaaS de pago y la nuestra es la tienda.
