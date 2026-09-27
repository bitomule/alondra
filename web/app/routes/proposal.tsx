import { Link, useParams } from 'react-router';
import { Lark } from '../public/Lark';
import { Shot } from '../public/Shot';
import { WaitlistForm, useWaitlist } from '../public/WaitlistForm';
import '../public/public.css';
import '../public/proposal.css';

export function meta({ params }: { params: { locale?: string } }) {
  const es = params.locale === 'es';
  return [
    { title: es ? 'Alondra — El orquestador para tus agentes' : 'Alondra — The orchestrator for your agents' },
    { name: 'description', content: es ? 'Todo en todas partes. Estamos construyendo un orquestador para personas y equipos que trabajan con agentes. Forma parte desde el principio.' : 'Everything, everywhere. We’re building an orchestrator for individuals and teams working with agents. Be part of it from the start.' },
    { name: 'robots', content: 'noindex, nofollow' },
  ];
}

const copy = {
  es: {
    vision: 'La visión', access: 'Apuntarme a la alpha',
    badge: 'Estamos construyendo Alondra', title: 'El orquestador para tus agentes.',
    intro: 'Escala tu capacidad de trabajar con agentes. Estamos construyendo el harness que te permite dirigir más trabajo con contexto, coordinación y control.',
    note: 'Únete a la lista de espera. Forma parte desde el principio.',
    graphic: 'Visión: tú o tu equipo dais la dirección. Alondra aporta el harness y la orquestación para coordinar agentes en paralelo, en distintos modelos y máquinas.',
    director: 'Tú / Tu equipo', direction: 'Dirección y criterio', harness: 'Harness + Orquestador', coordination: 'Contexto · Coordinación · Control', agents: 'Agentes trabajando en paralelo', distributed: 'Distintos modelos. Distintas máquinas.', scale: 'Más capacidad. Una misma dirección.',
    everywhere: 'Todo en todas partes.',
    everywhereBody: 'Imagina que todo ese trabajo repartido entre agentes, máquinas y personas tuviera un lugar en común.',
    everywhereClose: 'Eso es lo que estamos construyendo.',
    solo: 'Para lo que quieres construir tú.', team: 'Para lo que podéis construir juntos.',
    together: 'Queremos que tu capacidad crezca con tus agentes. Que puedas pasar de trabajar con uno a dirigir muchos, por tu cuenta o con tu equipo, sin perder el control del conjunto.',
    judgment: 'Más capacidad para crear. Más claridad para decidir.',
    judgmentBody: 'Los agentes pueden producir más trabajo del que podemos revisar. El siguiente paso es poder entenderlo, darle dirección y confiar en lo que llega a nuestras manos.',
    judgmentClose: 'Queremos que trabajar con agentes se sienta así.',
    started: 'Ya hemos empezado.',
    startedBody: 'La primera alpha conecta el trabajo de los agentes con tus decisiones. Es el comienzo de una visión que queremos construir contigo.',
    capture: 'Un vistazo a la alpha actual. Captura real con datos de ejemplo.',
    questionAlt: 'Alpha actual de Alondra: una pregunta de un agente con contexto y opciones para responder',
    alpha: 'Lo que viene empieza contigo.',
    alphaBody: 'Si ya imaginas lo que podrías construir con tus agentes, queremos que formes parte.',
    requirementsTitle: '¿Qué podré probar primero?',
    requirements: 'La primera alpha funciona con Claude Code en un Mac encendido. Los clientes de Mac y iPhone se distribuyen por TestFlight. La visión para equipos, otros modelos y entornos distribuidos se desarrollará a partir de esta base; aún no está disponible en la alpha.',
    invitation: 'Alpha privada. Invitaciones en tandas pequeñas.',
    privacy: 'Privacidad', privacyNote: 'En español', footer: 'Todo en todas partes.',
  },
  en: {
    vision: 'The vision', access: 'Join the alpha',
    badge: 'We’re building Alondra', title: 'The orchestrator for your agents.',
    intro: 'Scale your ability to work with agents. We’re building the harness to help you direct more work with context, coordination and control.',
    note: 'Join the waitlist. Be part of it from the start.',
    graphic: 'Vision: you or your team set the direction. Alondra provides the harness and orchestration to coordinate parallel agents across models and machines.',
    director: 'You / Your team', direction: 'Direction and judgment', harness: 'Harness + Orchestrator', coordination: 'Context · Coordination · Control', agents: 'Agents working in parallel', distributed: 'Different models. Different machines.', scale: 'More capacity. One shared direction.',
    everywhere: 'Everything, everywhere.',
    everywhereBody: 'Imagine all that work, spread across agents, machines and people, having a place in common.',
    everywhereClose: 'That’s what we’re building.',
    solo: 'For what you want to build.', team: 'For what you can build together.',
    together: 'We want your capacity to grow with your agents. To go from working with one to directing many, on your own or with your team, while keeping the whole picture in hand.',
    judgment: 'More power to create. More clarity to decide.',
    judgmentBody: 'Agents can produce more work than we can review. The next step is being able to understand it, give it direction and trust what reaches our hands.',
    judgmentClose: 'That’s how we want working with agents to feel.',
    started: 'We’ve already started.',
    startedBody: 'The first alpha connects the agents’ work with your decisions. It’s the beginning of a vision we want to build with you.',
    capture: 'A glimpse of the current alpha. Real screenshot with example data, in Spanish.',
    questionAlt: 'Current Alondra alpha: an agent’s question with context and response options',
    alpha: 'What comes next starts with you.',
    alphaBody: 'If you can already imagine what you could build with your agents, we’d love you to be part of it.',
    requirementsTitle: 'What can I try first?',
    requirements: 'The first alpha runs with Claude Code on a powered-on Mac. Mac and iPhone clients are distributed through TestFlight. The vision for teams, other models and distributed environments will grow from this foundation; it is not yet available in the alpha.',
    invitation: 'Private alpha. Invitations in small batches.',
    privacy: 'Privacy', privacyNote: 'In Spanish', footer: 'Everything, everywhere.',
  },
};

export default function Proposal() {
  const locale = useParams().locale === 'es' ? 'es' : 'en';
  const c = copy[locale];
  const waitlist = useWaitlist();
  return <div className="pub proposal" lang={locale}>
    <header className="p-header p-wrap">
      <Link to={`/proposal/${locale}`} className="pub-brand"><Lark className="pub-brand-mark"/><span>Alondra</span></Link>
      <nav aria-label={locale === 'es' ? 'Principal' : 'Main'}>
        <a className="p-nav-detail" href="#vision">{c.vision}</a>
        <div className="p-languages" aria-label={locale === 'es' ? 'Idioma' : 'Language'}><Link to="/proposal/en" lang="en" aria-label="English" aria-current={locale === 'en' ? 'page' : undefined}>EN</Link><Link to="/proposal/es" lang="es" aria-label="Español" aria-current={locale === 'es' ? 'page' : undefined}>ES</Link></div>
        <a className="p-nav-cta" href="#alpha">{c.access}</a>
      </nav>
    </header>
    <main id="main">
      <section className="p-hero p-wrap">
        <div className="p-hero-copy"><p className="p-badge">{c.badge}</p><h1>{c.title}</h1><p className="p-intro">{c.intro}</p>
          <WaitlistForm waitlist={waitlist} name="proposal-hero" locale={locale} submitLabel={c.access}/><p className="p-note">{c.note}</p>
        </div>
        <figure className="v-harness" role="img" aria-label={c.graphic}>
          <div className="v-director" aria-hidden="true"><strong>{c.director}</strong><span>{c.direction}</span></div>
          <svg className="v-input-wire" viewBox="0 0 400 40" aria-hidden="true"><path d="M190 0 V38 M210 38 V0 M186 32 L190 38 L194 32 M206 6 L210 0 L214 6"/></svg>
          <div className="v-harness-core" aria-hidden="true"><Lark className="v-harness-mark"/><strong>Alondra</strong><span className="v-harness-role">{c.harness}</span><span className="v-harness-capabilities">{c.coordination}</span></div>
          <svg className="v-fan" viewBox="0 0 400 68" preserveAspectRatio="none" aria-hidden="true"><path d="M200 0 V24 M50 66 V24 H350 V66 M150 24 V66 M250 24 V66"/><path className="v-fan-arrows" d="M45 60 L50 66 L55 60 M145 60 L150 66 L155 60 M245 60 L250 66 L255 60 M345 60 L350 66 L355 60"/></svg>
          <div className="v-agent-row" aria-hidden="true">{[0, 1, 2, 3].map(n => <div className="v-agent" key={n}><svg viewBox="0 0 48 48"><path d="M16 10 L4 24 L16 38 M32 10 L44 24 L32 38"/><circle cx="24" cy="24" r="4"/></svg></div>)}</div>
          <div className="v-agent-caption" aria-hidden="true"><strong>{c.agents}</strong><span>{c.distributed}</span></div>
          <figcaption aria-hidden="true">{c.scale}</figcaption>
        </figure>
      </section>
      <section className="v-vision" id="vision"><div className="p-wrap">
        <h2><span>{locale === 'es' ? 'Todo en' : 'Everything,'}</span><span>{locale === 'es' ? 'todas partes.' : 'everywhere.'}</span></h2>
        <div className="v-vision-copy"><p>{c.everywhereBody}</p><p>{c.everywhereClose}</p></div>
        <div className="v-together"><div><p>{c.solo}</p><p>{c.team}</p></div><p>{c.together}</p></div>
      </div></section>
      <section className="v-judgment p-wrap"><h2>{c.judgment}</h2><div><p>{c.judgmentBody}</p><p className="v-judgment-close">{c.judgmentClose}</p></div></section>
      <section className="v-proof p-wrap"><figure><Shot name="te-espera" tone="light" alt={c.questionAlt}/><figcaption>{c.capture}</figcaption></figure><div><h2>{c.started}</h2><p>{c.startedBody}</p></div></section>
      <section className="p-alpha" id="alpha"><div className="p-wrap p-alpha-grid"><div><h2>{c.alpha}</h2><p>{c.alphaBody}</p></div><div><WaitlistForm waitlist={waitlist} name="proposal-alpha" locale={locale} submitLabel={c.access}/><p className="p-note">{c.invitation}</p><details className="v-alpha-details"><summary>{c.requirementsTitle}</summary><p>{c.requirements}</p></details></div></div></section>
    </main>
    <footer className="p-footer p-wrap"><Link className="pub-brand" to={`/proposal/${locale}`}>Alondra</Link><p>{c.footer}</p><Link to="/privacy">{c.privacy} <span className="p-note">({c.privacyNote})</span></Link></footer>
  </div>;
}
