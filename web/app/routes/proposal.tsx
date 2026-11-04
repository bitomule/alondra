import { Link, useParams, useLocation } from 'react-router';
import { Lark } from '../public/Lark';
import { WaitlistForm, useWaitlist } from '../public/WaitlistForm';
import '../public/public.css';
import '../public/proposal.css';

export function meta({ params, location }: { params: { locale?: string }; location: { pathname: string; search: string } }) {
  const preview = location.pathname.startsWith('/proposal/');
  const es = (preview ? params.locale : new URLSearchParams(location.search).get('lang')) !== 'en';
  return [
    { title: es ? 'Alondra — El orquestador para tus agentes' : 'Alondra — The orchestrator for your agents' },
    { name: 'description', content: es ? 'El orquestador para tus agentes. Alondra está en construcción. Apúntate a la alpha privada.' : 'The orchestrator for your agents. Alondra is in the making. Join the private alpha waitlist.' },
    ...(preview ? [{ name: 'robots', content: 'noindex, nofollow' }] : [{ tagName: 'link', rel: 'canonical', href: `https://getalondra.com/${es ? '' : '?lang=en'}` }]),
  ];
}

const copy = {
  es: {
    title: 'El orquestador para tus agentes.', intro: 'Tú marcas la dirección.',
    idea: 'La idea', you: 'Tú', agents: 'Tus agentes',
    graphic: 'Concepto de orquestación: tú das la dirección a Alondra, que conecta con varios agentes.',
    alpha: 'Estamos construyéndolo.', access: 'Apuntarme a la alpha', badge: 'Alpha privada',
    privacy: 'Privacidad', footer: 'Todo en todas partes.',
  },
  en: {
    title: 'The orchestrator for your agents.', intro: 'You set the direction.',
    idea: 'The idea', you: 'You', agents: 'Your agents',
    graphic: 'Orchestration concept: you give direction to Alondra, which connects to several agents.',
    alpha: 'In the making.', access: 'Join the alpha', badge: 'Private alpha',
    privacy: 'Privacy', footer: 'Everything, everywhere.',
  },
};

// Concept illustration, deliberately separate from the actual alpha interface.
function Orchestra({ mobile = false }: { mobile?: boolean }) {
  const center = mobile ? { x: 200, y: 200 } : { x: 440, y: 190 };
  const person = mobile ? { x: 200, y: 45 } : { x: 100, y: 190 };
  const agents = mobile
    ? [{ x: 55, y: 350 }, { x: 150, y: 350 }, { x: 250, y: 350 }, { x: 345, y: 350 }]
    : [{ x: 725, y: 70 }, { x: 855, y: 145 }, { x: 725, y: 235 }, { x: 855, y: 310 }];
  const input = mobile ? 'M200 73 V144' : 'M128 190 H384';
  const paths = agents.map(a => mobile
    ? `M200 256 V280 Q200 300 ${a.x} 300 V324`
    : `M496 190 H550 C620 190 625 ${a.y} ${a.x - 26} ${a.y}`);
  return <svg className={mobile ? 'o-drawing o-mobile' : 'o-drawing o-desktop'} viewBox={mobile ? '0 0 400 395' : '0 0 960 380'} aria-hidden="true">
    <g className="o-wires"><path d={input}/>{paths.map((d, i) => <path key={i} d={d}/>)}</g>
    <g className="o-signals"><path d={input}/>{paths.map((d, i) => <path key={i} d={d} style={{ animationDelay: `${i * -.7}s` }}/>)}</g>
    <g transform={`translate(${person.x} ${person.y})`} className="o-person"><circle r="28"/><circle cy="-6" r="6"/><path d="M-12 14 C-12 0 12 0 12 14"/></g>
    <g transform={`translate(${center.x} ${center.y})`} className="o-core"><rect x="-56" y="-56" width="112" height="112" rx="24"/><path className="o-brackets" d="M-66 -38 H-74 V38 H-66 M66 -38 H74 V38 H66"/><svg x="-39" y="-28" width="78" height="56"><Lark/></svg></g>
    {agents.map((a, i) => <g key={i} transform={`translate(${a.x} ${a.y})`} className="o-agent"><rect x="-26" y="-26" width="52" height="52" rx="12"/><path d="M-8 -9 L-17 0 L-8 9 M8 -9 L17 0 L8 9"/><circle r="2.5"/></g>)}
  </svg>;
}

export default function Proposal() {
  const params = useParams();
  const location = useLocation();
  const preview = location.pathname.startsWith('/proposal/');
  const locale = (preview ? params.locale : new URLSearchParams(location.search).get('lang')) === 'en' ? 'en' : 'es';
  const localeUrl = (language: string) => preview ? `/proposal/${language}` : (language === 'es' ? '/' : '/?lang=en');
  const c = copy[locale];
  const waitlist = useWaitlist();
  return <div className="pub proposal" lang={locale}>
    <header className="p-header p-wrap">
      <Link to={localeUrl(locale)} className="pub-brand" aria-label={locale === 'es' ? 'Alondra, inicio' : 'Alondra, home'}><Lark className="pub-brand-mark"/><span>Alondra</span></Link>
      <nav aria-label={locale === 'es' ? 'Idioma' : 'Language'} className="p-languages"><Link to={localeUrl('en')} lang="en" aria-label="English" aria-current={locale === 'en' ? 'page' : undefined}>EN</Link><Link to={localeUrl('es')} lang="es" aria-label="Español" aria-current={locale === 'es' ? 'page' : undefined}>ES</Link></nav>
    </header>
    <main id="main">
      <section className="p-hero p-wrap"><h1>{c.title}</h1><p className="p-intro">{c.intro}</p><a className="p-hero-link" href="#alpha">{c.access}<span aria-hidden="true"> ↗</span></a></section>
      <figure className="v-harness p-wrap" id="vision" role="img" aria-label={c.graphic}>
        <div className="o-heading" aria-hidden="true"><span>{c.idea}</span></div>
        <div className="o-canvas" aria-hidden="true"><Orchestra/><Orchestra mobile/><div className="o-labels"><span>{c.you}</span><span>Alondra</span><span>{c.agents}</span></div></div>
        <figcaption aria-hidden="true">{c.footer}</figcaption>
      </figure>
      <section className="p-alpha p-wrap" id="alpha"><div className="p-alpha-title"><p className="p-badge">{c.badge}</p><h2>{c.alpha}</h2></div><div className="p-alpha-form"><WaitlistForm waitlist={waitlist} name="proposal-alpha" locale={locale} submitLabel={c.access}/></div></section>
    </main>
    <footer className="p-footer p-wrap"><Link className="pub-brand" to={localeUrl(locale)}>Alondra</Link><nav aria-label={locale === 'es' ? 'Enlaces' : 'Links'}><Link to="/install">{locale === 'es' ? 'Instalar' : 'Install (ES)'}</Link><a href="/auth/login">{locale === 'es' ? 'Entrar' : 'Sign in'}</a><Link to="/privacy">{c.privacy}</Link></nav></footer>
  </div>;
}
