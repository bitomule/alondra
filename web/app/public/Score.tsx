import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { Lark } from './Lark';

const END_MS = 10400;

type Play = 'static' | 'armed' | 'playing' | 'done';

// An element that appears at `at` seconds and, if `until` is given, leaves again: it is not part of the final frame.
function k(at: number, until?: number, extra = ''): { className: string; style: CSSProperties; 'data-temp'?: '' } {
  const style = { '--in': `${at}s`, ...(until === undefined ? {} : { '--out': `${until}s` }) } as CSSProperties;
  return until === undefined ? { className: `k ${extra}`.trim(), style } : { className: `k ${extra}`.trim(), style, 'data-temp': '' };
}

function Swap({ children }: { children: ReactNode }) {
  return <span className="swap">{children}</span>;
}

function Status({ tone, at, until, children }: { tone: 'run' | 'wait' | 'ask' | 'done' | 'pr'; at: number; until?: number; children: ReactNode }) {
  return <span {...k(at, until, `fade status is-${tone}`)}><i aria-hidden="true"/>{children}</span>;
}

function Tap({ at }: { at: number }) {
  return <span className="tap" style={{ '--in': `${at}s` } as CSSProperties}/>;
}

function Lane({ id, title, agent, top, at, children, chip }: { id: string; title: string; agent: string; top: number; at: number; children: ReactNode; chip: ReactNode }) {
  return (
    <div className="score-node score-lane" style={{ top: `${top}em` }}>
      <div {...k(at, undefined, 'lane-card')}>
        <p className="lane-head"><span className="lane-id">{id}</span><span className="lane-title">{title}</span></p>
        <p className="lane-agent"><span className="lane-agent-name">{agent}</span><Swap>{children}</Swap></p>
        <span className="lane-chip"><Swap>{chip}</Swap></span>
      </div>
    </div>
  );
}

function Wire({ d, at }: { d: string; at: number }) {
  return <path className="wire" d={d} pathLength={1} style={{ '--in': `${at}s` } as CSSProperties}/>;
}

export function Score() {
  const ref = useRef<HTMLDivElement>(null);
  const [play, setPlay] = useState<Play>('static');
  const [paused, setPaused] = useState(false);
  const clock = useRef<{ started: number; elapsed: number; timer?: number }>({ started: 0, elapsed: 0 });

  useEffect(() => {
    const node = ref.current;
    const reduce = window.matchMedia('(prefers-reduced-motion: reduce)');
    if (!node || reduce.matches || !('IntersectionObserver' in window)) return;
    setPlay('armed');
    const observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) { observer.disconnect(); start(); }
    }, { threshold: 0.35 });
    observer.observe(node);
    const onReduce = () => { if (reduce.matches) { observer.disconnect(); stop('static'); } };
    reduce.addEventListener('change', onReduce);
    return () => { observer.disconnect(); reduce.removeEventListener('change', onReduce); window.clearTimeout(clock.current.timer); };
  }, []);

  function schedule(remaining: number) {
    window.clearTimeout(clock.current.timer);
    clock.current.started = performance.now();
    clock.current.timer = window.setTimeout(() => stop('done'), remaining);
  }
  function start() {
    clock.current.elapsed = 0;
    setPaused(false);
    setPlay('playing');
    schedule(END_MS);
  }
  function stop(next: Play) {
    window.clearTimeout(clock.current.timer);
    setPaused(false);
    setPlay(next);
  }
  function togglePause() {
    if (paused) { setPaused(false); schedule(END_MS - clock.current.elapsed); return; }
    window.clearTimeout(clock.current.timer);
    clock.current.elapsed += performance.now() - clock.current.started;
    setPaused(true);
  }
  function replay() {
    setPlay('armed');
    requestAnimationFrame(() => requestAnimationFrame(start));
  }

  return (
    <figure className="score" aria-labelledby="score-title">
      <div className="score-intro">
        <h2 id="score-title" className="pub-h2">Una petición, de principio a fin.</h2>
        <p className="score-lede">Pides una vez. El orquestador reparte, los agentes trabajan en tu Mac y a ti solo te llega lo que necesita una decisión.</p>
        <div className="score-controls">
          {play === 'playing' && <button type="button" className="score-button" onClick={togglePause} aria-pressed={paused}>{paused ? 'Reanudar' : 'Pausar'}</button>}
          {play === 'done' && <button type="button" className="score-button" onClick={replay}>Ver otra vez</button>}
        </div>
      </div>
      <div className="score-stage">
        <div ref={ref} className="score-canvas" data-play={play} data-paused={paused || undefined} aria-hidden="true">
          <p className="score-col" style={{ left: '0em', width: '16em' }}><b>1</b>Pides</p>
          <p className="score-col" style={{ left: '19.5em', width: '11em' }}><b>2</b>Orquestador</p>
          <p className="score-col" style={{ left: '34em', width: '26em' }}><b>3</b>Agentes en tu Mac</p>
          <p className="score-col" style={{ left: '64em', width: '18em' }}><b>4</b>Tú decides</p>

          <svg className="score-wires" viewBox="0 0 82 28" preserveAspectRatio="none">
            <Wire d="M16 16.3 L20.9 16.3" at={1.2}/>
            <Wire d="M26.3 16.3 C30 16.3 30.5 7.5 34 7.5" at={2.3}/>
            <Wire d="M26.3 16.3 L34 16.5" at={2.4}/>
            <Wire d="M26.3 16.3 C30 16.3 30.5 25.5 34 25.5" at={2.5}/>
            <Wire d="M60 7.5 C62 7.5 62 10 64 10" at={5.2}/>
            <Wire d="M60 16.5 C62 16.5 62 18 64 18" at={5.8}/>
            <Wire d="M60 25.5 C62 25.5 62 24.3 64 24.3" at={7.3}/>
          </svg>

          <div className="score-node score-request" style={{ top: '11em' }}>
            <div {...k(0, undefined, 'req-card')}>
              <p className="req-head"><span>Nuevo</span><span className="req-where">desde el iPhone</span></p>
              <p className="req-text">Exporta los informes a CSV y cubre el formato de fechas con tests.</p>
              <p className="req-chips"><span className="chip chip-issue">informes#42</span><span className="chip">Revisar cada PR</span><span className="chip">Normal</span></p>
              <p className="req-foot"><span className="req-send press" style={{ '--in': '1s' } as CSSProperties}>Enviar<Tap at={1}/></span></p>
            </div>
          </div>

          <div className="score-node score-conductor" style={{ top: '13.5em' }}>
            <Lark className="score-lark"/>
            <p className="conductor-name">Orquestador</p>
            <p className="conductor-state"><Swap>
              <span {...k(1.6, 2.4, 'fade')}>lee la petición</span>
              <span {...k(2.4, undefined, 'fade')}>3 tareas, 1 dependencia</span>
            </Swap></p>
          </div>

          <Lane id="t-1" title="Modelo de exportación" agent="informes/export" top={4} at={2.6} chip={<>
            <span {...k(5, 8.4, 'pop chip chip-pr')}>PR #212</span>
            <span {...k(8.4, undefined, 'pop chip chip-merged')}>#212 fusionado</span>
          </>}>
            <Status tone="run" at={3} until={5}>en marcha</Status>
            <Status tone="pr" at={5} until={8.4}>abre su PR</Status>
            <Status tone="done" at={8.4}>terminada</Status>
          </Lane>
          <Lane id="t-2" title="Tests del formato de fecha" agent="informes/tests" top={13} at={2.75} chip={<>
            <span {...k(5.6, 9.3, 'pop chip chip-ask')}>pregunta</span>
            <span {...k(9.3, undefined, 'pop chip')}>respondida</span>
          </>}>
            <Status tone="run" at={3} until={5.6}>en marcha</Status>
            <Status tone="ask" at={5.6} until={9.3}>te pregunta</Status>
            <Status tone="run" at={9.3}>sigue sola</Status>
          </Lane>
          <Lane id="t-3" title="Botón Exportar en Informes" agent="informes/boton" top={22} at={2.9} chip={<>
            <span {...k(7.2, undefined, 'pop chip chip-pr')}>PR #213</span>
          </>}>
            <Status tone="wait" at={3} until={5.1}>espera a t-1</Status>
            <Status tone="run" at={5.1} until={7.2}>en marcha</Status>
            <Status tone="pr" at={7.2}>encima de #212</Status>
          </Lane>

          <div className="score-node score-inbox" style={{ top: '4em' }}>
            <p {...k(5.5, undefined, 'inbox-head')}>Te espera <Swap>
              <span {...k(5.6, 6.2, 'fade count')}>1</span>
              <span {...k(6.2, 7.4, 'fade count')}>2</span>
              <span {...k(7.4, 8.3, 'fade count')}>3</span>
              <span {...k(8.3, 9.3, 'fade count')}>2</span>
              <span {...k(9.3, undefined, 'fade count')}>1</span>
            </Swap></p>
            <div {...k(5.6, undefined, 'card card-pr')}>
              <p className="card-kind">PR #212 · t-1</p>
              <p className="card-title">Exportar informes a CSV</p>
              <p className="card-meta"><span className="diff-add">+184</span> <span className="diff-del">−12</span> · 6 ficheros · <span className="checks">checks ✓</span></p>
              <Swap>
                <span {...k(5.6, 8.2, 'fade card-actions')}><span className="btn btn-primary press" style={{ '--in': '8s' } as CSSProperties}>Fusionar<Tap at={8}/></span><span className="btn">Pedir cambios</span></span>
                <span {...k(8.2, undefined, 'fade card-result')}>Fusionado con squash</span>
              </Swap>
            </div>
            <div {...k(6.2, undefined, 'card card-ask')}>
              <p className="card-kind">Pregunta · t-2</p>
              <p className="card-title">¿Mantengo las fechas en ISO 8601?</p>
              <Swap>
                <span {...k(6.2, 9.2, 'fade card-actions')}><span className="btn btn-primary press" style={{ '--in': '9s' } as CSSProperties}>Sí, ISO 8601<Tap at={9}/></span><span className="btn">Formato local</span></span>
                <span {...k(9.2, undefined, 'fade card-result')}>Enviado a su nodo</span>
              </Swap>
            </div>
            <div {...k(7.4, undefined, 'card card-mini')}>
              <p className="card-kind">PR #213 · t-3</p>
              <p className="card-title">Botón Exportar, va encima de #212</p>
            </div>
          </div>
        </div>
      </div>
      <ol className="sr-only">
        <li>Pides trabajo en Nuevo, por ejemplo desde el iPhone y a partir de una issue.</li>
        <li>El orquestador lo reparte en tres tareas, una de ellas depende de otra.</li>
        <li>Tres agentes trabajan en tu Mac: dos abren PR y uno te hace una pregunta.</li>
        <li>Te llegan como tarjetas. Fusionas el primer PR y respondes la pregunta; el agente sigue solo.</li>
      </ol>
    </figure>
  );
}
