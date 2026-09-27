import { Link, Outlet, useLocation } from 'react-router';
import { Lark } from '../public/Lark';
import '../public/public.css';

export default function PublicLayout() {
  const home = useLocation().pathname === '/';
  return (
    <div className={`pub${home ? ' pub-home' : ''}`}>
      <header className="pub-header">
        <Link className="pub-brand" to="/" aria-label="Alondra, inicio"><Lark className="pub-brand-mark"/><span>Alondra</span></Link>
        <nav className="pub-nav" aria-label="Principal">
          <a className="pub-nav-extra" href="/#como-funciona">Cómo funciona</a>
          <Link className="pub-nav-extra" to="/privacy">Privacidad</Link>
          <Link className="pub-nav-extra" to="/install">Instalar</Link>
          <a className="pub-nav-login" href="/auth/login">Entrar</a>
          <a className="pub-nav-cta" href="/#acceso">Pedir acceso</a>
        </nav>
      </header>
      <main id="main"><Outlet/></main>
      <footer className="pub-footer">
        <p className="pub-footer-line">Alondra es el orquestador. Issues, tareas y agentes son solo el principio.</p>
        <div className="pub-footer-meta">
          <Link className="pub-brand pub-brand-small" to="/"><Lark className="pub-brand-mark"/><span>Alondra</span></Link>
          <nav aria-label="Pie de página">
            <a href="/#como-funciona">Cómo funciona</a>
            <Link to="/privacy">Privacidad</Link>
            <Link to="/install">Instalar</Link>
            <a href="/auth/login">Entrar</a>
          </nav>
          <p>Alpha privada. Sin nube que ejecute agentes, sin equipos, sin facturación.</p>
        </div>
      </footer>
    </div>
  );
}
