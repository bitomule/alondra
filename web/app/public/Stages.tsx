import { useEffect, useRef, useState, type CSSProperties } from 'react';
import { Shot } from './Shot';

type Note = { text: string; y: number };
type Stage = { id: string; title: string; body: string; facts: string[]; shot: string; back?: string; alt: string; notes: Note[] };

const STAGES: Stage[] = [
  {
    id: 'pides', title: 'Pides. Como se lo dirías a alguien del equipo.',
    body: 'Escribe lo que quieres en Nuevo, desde el iPhone, la app de Mac o el terminal, y adjunta fotos o archivos si ayudan. O parte de una issue abierta de tus repos: el orquestador recibe su texto y sus comentarios, y el PR la cerrará con «Fixes».',
    facts: ['Proyecto: lo eliges tú o el orquestador', 'PR: revisar cada una o merge automático', 'Esfuerzo: ligero, normal o a fondo'],
    shot: 'nuevo', back: 'opciones', alt: 'Pantalla Nuevo de Alondra en el iPhone con una petición escrita y sus opciones',
    notes: [{ text: 'Tu petición, con adjuntos', y: 30 }, { text: 'Proyecto, issue, PR y esfuerzo', y: 64 }],
  },
  {
    id: 'reparte', title: 'El orquestador lo parte en tareas.',
    body: 'El orquestador es una conversación de Claude que Alondra mantiene viva. Lee tu petición, la divide en tareas con sus dependencias —esta va encima del PR de aquella— y elige proyecto, modelo y esfuerzo para cada una. Antes de lanzar agentes mira la memoria del Mac y tu ritmo de uso de Claude.',
    facts: ['Dependencias entre tareas', 'PR en orden de fusión', 'Si la máquina no da para más, espera'],
    shot: 'tarea', alt: 'Detalle de una petición en Alondra: sus tareas, los PR en orden de fusión y lo que te espera',
    notes: [{ text: 'Lo que te espera de esta tarea', y: 52 }, { text: 'PR en orden de fusión', y: 90 }],
  },
  {
    id: 'flota', title: 'Los agentes trabajan solos, en tu Mac.',
    body: 'Cada tarea la hace un agente de Claude Code en segundo plano, con tus repos y tus herramientas. No hay prompts escondidos en un terminal que no miras: si necesita algo, te lo pregunta como tarjeta. Si se atasca o termina sin decir qué quiere, el orquestador lo detecta y lo reencamina.',
    facts: ['trabajando · esperando máquina · te espera', 'Uso de Claude y memoria del Mac a la vista'],
    shot: 'flota', back: 'tareas', alt: 'Pestaña Flota de Alondra con un agente, su estado y lo último que dijo',
    notes: [{ text: 'Busca por nodo, proyecto o tarea', y: 21 }, { text: 'Cada agente, su estado y lo último que dijo', y: 33 }],
  },
  {
    id: 'tarjetas', title: 'Te llega solo lo que necesita una decisión.',
    body: 'Una tarjeta por tarea: el PR con su diff y sus checks, una pregunta con opciones, un permiso, un documento para comentar por bloques o un aviso. Llegan como notificación, y puedes responder, permitir o aprobar desde la pantalla bloqueada.',
    facts: ['Pregunta · Permiso · PR · Documento · Aviso', 'Responde con un toque o con tus palabras'],
    shot: 'te-espera', back: 'documento', alt: 'Pestaña Te espera de Alondra con una pregunta de un agente y sus opciones',
    notes: [{ text: 'Quién pregunta y para qué tarea', y: 48 }, { text: 'Opciones o tu propia respuesta', y: 70 }],
  },
  {
    id: 'decides', title: 'Decides. El agente sigue.',
    body: 'Fusiona con squash, rebase o merge; pide cambios comentando líneas del diff; o aparta la tarjeta para después. Tu respuesta se escribe en la sesión del agente, que sigue trabajando. Cuando se fusiona su PR y no le queda nada, la tarea se cierra.',
    facts: ['Comentarios por línea del diff', 'Squash · Rebase · Merge', 'Apartar para después'],
    shot: 'pr', alt: 'Un PR en Alondra: checks, diff con líneas añadidas y quitadas y el botón Fusionar',
    notes: [{ text: 'Toca una línea para comentarla', y: 52 }, { text: 'Fusionar o pedir cambios', y: 89 }],
  },
];

function Visual({ stage }: { stage: Stage }) {
  return (
    <div className="stage-visual">
      <div className="stage-device">
        {stage.back && <Shot name={stage.back} alt="" className="stage-shot stage-shot-back"/>}
        <Shot name={stage.shot} alt={stage.alt} className="stage-shot"/>
        {stage.notes.map((note, i) => (
          <p key={note.text} className="stage-note" style={{ '--y': `${note.y}%`, '--i': i } as CSSProperties} aria-hidden="true">{note.text}</p>
        ))}
      </div>
    </div>
  );
}

export function Stages() {
  const [active, setActive] = useState(0);
  const refs = useRef<(HTMLElement | null)[]>([]);

  useEffect(() => {
    if (!('IntersectionObserver' in window)) return;
    const observer = new IntersectionObserver(entries => {
      for (const entry of entries) if (entry.isIntersecting) setActive(Number((entry.target as HTMLElement).dataset.index));
    }, { rootMargin: '-45% 0px -45% 0px' });
    refs.current.forEach(node => node && observer.observe(node));
    return () => observer.disconnect();
  }, []);

  return (
    <section className="stages" id="como-funciona" aria-labelledby="stages-title">
      <div className="stages-head">
        <h2 id="stages-title" className="pub-h2">Tú pides y tú decides. Lo de en medio lo hace la flota.</h2>
      </div>
      <div className="stages-body">
        <ol className="stages-list">
          {STAGES.map((stage, index) => (
            <li key={stage.id} className="stage" data-index={index} data-active={active === index || undefined} ref={node => { refs.current[index] = node; }}>
              <p className="stage-step" aria-hidden="true">{String(index + 1).padStart(2, '0')}</p>
              <h3 className="stage-title">{stage.title}</h3>
              <p className="stage-text">{stage.body}</p>
              <ul className="stage-facts">{stage.facts.map(fact => <li key={fact}>{fact}</li>)}</ul>
              <div className="stage-inline"><Visual stage={stage}/></div>
            </li>
          ))}
        </ol>
        <div className="stages-screen" aria-hidden="true">
          <div className="stages-sticky">
            {STAGES.map((stage, index) => (
              <div key={stage.id} className="stages-frame" data-active={active === index || undefined}><Visual stage={stage}/></div>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
