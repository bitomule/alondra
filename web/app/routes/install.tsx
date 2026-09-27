import { Lark } from '../public/Lark';

export function meta() { return [{ title: 'Instalar — Alondra' }, { name: 'description', content: 'Instala Alondra en tu Mac con Homebrew, prepáralo con alondra setup y vincula el iPhone o la app de Mac.' }]; }

const STEPS = [
  { title: 'Instala Alondra', commands: ['brew install bitomule/tap/alondra'], body: 'Incluye el servicio, la CLI y la TUI. Las apps de iPhone y de Mac se instalan aparte.' },
  { title: 'Prepara tu Mac', commands: ['alondra setup'], body: 'El asistente comprueba la instalación y te guía por el inicio de sesión y la configuración. Puedes repetirlo cuando quieras.' },
  { title: 'Vincula el iPhone o la app de Mac', commands: ['alondra pair'], body: 'Abre la app que te llega por TestFlight, escanea el código y confirma el emparejamiento en tu Mac. Durante la alpha, David te da acceso a TestFlight.' },
  { title: 'Comprueba y abre la TUI', commands: ['alondra doctor', 'alondra'], body: 'doctor te dice qué funciona, qué falta y cómo arreglarlo. alondra abre la TUI con Bandeja, Tareas, Programadas y Flota.' },
];

export default function Install() {
  return (
    <article className="doc">
      <header className="doc-head">
        <Lark className="doc-lark"/>
        <h1 className="doc-title">Empieza por tu Mac.</h1>
        <p className="doc-lede">Necesitas una invitación, un Mac con macOS y Homebrew, y Claude autorizado en el ordenador donde van a trabajar los agentes.</p>
      </header>
      <ol className="doc-steps">
        {STEPS.map(step => (
          <li key={step.title} className="doc-step">
            <h2>{step.title}</h2>
            {step.commands.map(command => <pre key={command}><code>{command}</code></pre>)}
            <p>{step.body}</p>
          </li>
        ))}
      </ol>
      <p className="doc-aside">¿Aún sin invitación? <a className="pub-link" href="/#acceso">Pide acceso a la alpha</a></p>
    </article>
  );
}
