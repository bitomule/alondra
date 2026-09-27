import { Link } from 'react-router';
import { Lark } from '../public/Lark';
import { Score } from '../public/Score';
import { Shot } from '../public/Shot';
import { Stages } from '../public/Stages';
import { Terminal } from '../public/Terminal';
import { WaitlistForm, useWaitlist } from '../public/WaitlistForm';

export function meta() {
  return [
    { title: 'Alondra — el orquestador de tus agentes de Claude' },
    { name: 'description', content: 'Pides trabajo, el orquestador lo parte en tareas y lanza agentes de Claude en tu Mac que abren PR. Te llega lo que necesita una decisión: en el iPhone, el Mac o el terminal. Alpha privada.' },
  ];
}

const AUTONOMY = [
  { label: 'permission-mode auto', title: 'Modo automático, sin prompts escondidos', body: 'Lo que el modo automático deniega aparece como fallo visible. Lo que necesita tu criterio te llega como pregunta, y el agente espera tu respuesta.' },
  { label: 'auto_merge', title: 'Merge automático, si lo quieres', body: 'Con merge automático, Alondra fusiona el PR en cuanto pasan los checks. Si no, cada PR es una tarjeta para ti.' },
  { label: 'depends_on', title: 'Dependencias que arrancan solas', body: 'Una tarea que va encima de otra espera a su PR y empieza cuando puede, sin que tengas que lanzarla.' },
  { label: 'stalled', title: 'Nadie se queda colgado', body: 'Un agente parado, que tarda demasiado o que termina sin decir qué quiere no se queda ahí: el orquestador lo reencamina o te pregunta.' },
  { label: 'over_pace', title: 'Ritmo y máquina bajo control', body: 'Mira tu uso semanal de Claude y la memoria del Mac. Si hoy vas por encima de tu parte de la semana, te pregunta antes de lanzar más agentes.' },
  { label: 'schedule', title: 'Programadas', body: 'Misiones recurrentes para el orquestador: cada día, cada semana o una sola vez, con sus opciones de lanzamiento.' },
];

export default function Home() {
  const waitlist = useWaitlist();
  return (
    <>
      <section className="hero" aria-labelledby="hero-title">
        <div className="hero-copy">
          <p className="hero-kicker"><span aria-hidden="true"/>El orquestador de tus agentes de Claude</p>
          <h1 id="hero-title" className="hero-title">
            <span>Todos tus agentes.</span>
            <span>En todas partes.</span>
            <span className="hero-title-accent">Con la máxima autonomía.</span>
          </h1>
          <div className="hero-split">
            <p className="hero-lede">Pides trabajo, o eliges una issue de GitHub. El orquestador lo parte en tareas y lanza agentes de Claude en tu Mac que trabajan solos hasta abrir su PR. Sus preguntas, informes y PR te llegan como tarjetas al iPhone, a la app de Mac o al terminal. Tú decides.</p>
            <div className="hero-form">
              <WaitlistForm waitlist={waitlist} name="hero" tone="stage"/>
              <p className="hero-note">Alpha privada con invitación. Necesitas un Mac encendido con Claude autorizado.</p>
            </div>
          </div>
        </div>
        <div className="wingspan" aria-hidden="true">
          <svg className="wingspan-cues" viewBox="0 0 1200 520" preserveAspectRatio="none">
            <path d="M548 131 C470 150 400 220 330 300"/>
            <path d="M548 131 C430 130 280 190 150 260"/>
            <path d="M652 131 C730 150 800 220 870 300"/>
            <path d="M652 131 C770 130 920 190 1050 260"/>
          </svg>
          <Lark className="wingspan-lark"/>
          <div className="wing wing-far-l"><Shot name="pr" alt="" tone="dark" eager/></div>
          <div className="wing wing-near-l"><Shot name="te-espera" alt="" tone="dark" eager/></div>
          <div className="wing wing-near-r"><Terminal name="tareas" label=""/></div>
          <div className="wing wing-far-r"><Shot name="tareas" alt="" tone="dark" eager/></div>
        </div>
      </section>

      <Score/>

      <Stages/>

      <section className="autonomy" aria-labelledby="autonomy-title">
        <div className="autonomy-head">
          <h2 id="autonomy-title" className="pub-h2">Máxima autonomía. La última palabra, tuya.</h2>
          <p>Alondra está hecha para que la flota avance sin ti y se pare solo donde hace falta.</p>
          <Lark className="autonomy-lark"/>
        </div>
        <ul className="autonomy-list">
          {AUTONOMY.map(item => (
            <li key={item.label}>
              <p className="autonomy-key" aria-hidden="true">{item.label}</p>
              <h3>{item.title}</h3>
              <p>{item.body}</p>
            </li>
          ))}
        </ul>
      </section>

      <section className="surfaces" aria-labelledby="surfaces-title">
        <h2 id="surfaces-title" className="pub-h2">En todas partes. La misma flota.</h2>
        <div className="surfaces-grid">
          <article className="surface surface-phone">
            <Shot name="te-espera" alt="La pestaña Te espera en el iPhone" className="surface-shot"/>
            <div>
              <h3>iPhone</h3>
              <p>Te espera, Tareas, Flota y Nuevo. Notificaciones con acciones: responde, permite o aprueba sin abrir la app.</p>
            </div>
          </article>
          <article className="surface surface-terminal">
            <Terminal name="bandeja" label="La TUI de Alondra en el terminal, pestaña Bandeja"/>
            <div>
              <h3>Terminal</h3>
              <p><code>alondra</code> abre la TUI con Bandeja, Tareas, Programadas y Flota. <code>alondra status</code> da lo mismo en texto.</p>
            </div>
          </article>
          <article className="surface surface-mac">
            <div>
              <h3>Mac</h3>
              <p>La app de Mac: la bandeja en el lateral, el diff a lo ancho y <kbd>⇧⌘N</kbd> para pedir trabajo nuevo.</p>
            </div>
          </article>
        </div>
      </section>

      <section className="privacy-band" aria-labelledby="privacy-title">
        <h2 id="privacy-title" className="pub-h2">Privacidad, dicha con precisión.</h2>
        <div className="privacy-grid">
          <div>
            <h3>Cifrado entre tus dispositivos</h3>
            <p>Lo que ves en el iPhone o en otro Mac —tarjetas, diffs, respuestas— viaja cifrado entre dispositivos autorizados. Los agentes se ejecutan en tu Mac, con tu propia cuenta de Claude.</p>
          </div>
          <div>
            <h3>Tu cuenta no abre tu Mac</h3>
            <p>Iniciar sesión no da acceso al contenido de ningún Mac. Para vincular un dispositivo hay que aprobarlo desde ese Mac.</p>
          </div>
          <div>
            <h3>Lo que sí ve el servicio</h3>
            <p>El servicio gestiona cuentas e invitaciones y ve metadatos, como destinos y tiempos. También guarda los emails de la lista de espera para poder invitarte.</p>
          </div>
        </div>
        <Link className="pub-link" to="/privacy">Todo el detalle de privacidad</Link>
      </section>

      <section className="access" id="acceso" aria-labelledby="access-title">
        <div className="access-copy">
          <h2 id="access-title" className="pub-h2">Alpha privada. Pocas plazas, requisitos reales.</h2>
          <ul className="access-list">
            <li><strong>Un Mac encendido con Claude autorizado.</strong> Ahí trabajan los agentes.</li>
            <li><strong>macOS y Homebrew.</strong> Instalan el servicio, la CLI y la TUI.</li>
            <li><strong>iPhone o app de Mac, si quieres.</strong> Durante la alpha llegan por TestFlight.</li>
            <li><strong>Una invitación.</strong> Las damos a mano, en tandas pequeñas.</li>
            <li><strong>Sin nube, equipos ni facturación.</strong> Tu Mac y tu cuenta de Claude.</li>
          </ul>
        </div>
        <div className="access-form">
          <WaitlistForm waitlist={waitlist} name="acceso"/>
          <p className="access-note">¿Ya tienes invitación? <a href="/auth/login">Entra</a> o sigue la <Link to="/install">instalación</Link>.</p>
        </div>
      </section>
    </>
  );
}
