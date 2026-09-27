import { Lark } from '../public/Lark';

export function meta() { return [{ title: 'Privacidad — Alondra' }, { name: 'description', content: 'Qué se queda en tu Mac, qué viaja cifrado entre tus dispositivos y qué conoce el servicio de Alondra.' }]; }

const SECTIONS = [
  { title: 'Qué se queda en tu Mac', body: 'Las tareas, los archivos y la ejecución de los agentes de Claude se quedan en tu ordenador. Lo que llega a tus otros dispositivos —tarjetas, diffs, respuestas— viaja cifrado entre dispositivos autorizados, que guardan las claves para leerlo.' },
  { title: 'Iniciar sesión no abre tu Mac', body: 'Tu cuenta no da acceso al contenido de ningún Mac. Para vincular un iPhone u otro Mac hay que aprobarlo desde ese Mac.' },
  { title: 'Qué conoce el servicio', body: 'Alondra gestiona cuentas, invitaciones, dispositivos y el transporte de mensajes. Ve metadatos como destinos, tiempos y tokens de notificaciones. El proveedor de acceso también procesa metadatos de conexión.' },
  { title: 'La lista de espera', body: 'Si te apuntas, el servicio guarda tu email para poder invitarte. Ese email no forma parte del contenido cifrado entre tus dispositivos: lo guarda y lo gestiona el servicio. No enviamos correos de confirmación.' },
  { title: 'Qué puede hacer un administrador', body: 'Invitarte o retirarte el acceso al servicio. La cuenta de administración no permite descifrar tus conversaciones ni recuperar las claves de tus dispositivos.' },
  { title: 'Retirar el acceso', body: 'Revocar el servicio no borra archivos del Mac ni garantiza retirar copias que ya tengan otros dispositivos. La confianza entre dispositivos se gestiona por separado.' },
  { title: 'Una alpha, con límites claros', body: 'Estamos probando Alondra con un grupo pequeño. No prometemos disponibilidad continua ni recuperación del contenido si pierdes tus claves.' },
];

export default function Privacy() {
  return (
    <article className="doc">
      <header className="doc-head">
        <Lark className="doc-lark"/>
        <h1 className="doc-title">Tu contenido. Tus dispositivos.</h1>
        <p className="doc-lede">La cuenta te deja usar el servicio. El emparejamiento te da acceso a un Mac. Son permisos distintos.</p>
      </header>
      <div className="doc-sections">
        {SECTIONS.map(section => (
          <section key={section.title} className="doc-section">
            <h2>{section.title}</h2>
            <p>{section.body}</p>
          </section>
        ))}
      </div>
    </article>
  );
}
