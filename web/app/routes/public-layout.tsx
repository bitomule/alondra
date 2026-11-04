import { Link, NavLink, Outlet, useLocation } from 'react-router';
import { Lark } from '../public/Lark';
import '../public/site.css';

const PAGES = [
  { to: '/how-it-works', label: 'How it works' },
  { to: '/install', label: 'Install' },
  { to: '/docs', label: 'Documentation' },
  { to: '/security', label: 'Security and privacy' },
  { to: '/license', label: 'License' },
];

function Links({ onClick }: { onClick?: () => void }) {
  return <>{PAGES.map(page => <NavLink key={page.to} to={page.to} onClick={onClick}>{page.label}</NavLink>)}</>;
}

export default function PublicLayout() {
  const home = useLocation().pathname === '/';
  return (
    <div className={`site${home ? ' site-home' : ''}`}>
      <header className="site-header">
        <div className="wrap site-header-row">
          <Link className="brand" to="/" aria-label="Alondra, home"><Lark className="brand-mark"/><span>Alondra</span></Link>
          <nav className="nav-wide" aria-label="Main"><Links/></nav>
          <div className="header-actions">
            <a className="signin" href="/auth/login">Sign in</a>
            <Link className="button button-small" to="/#cloud">Join the waitlist</Link>
          </div>
          <details className="menu">
            <summary>Menu</summary>
            <nav aria-label="Main, compact"><Links onClick={() => { (document.querySelector('.menu') as HTMLDetailsElement | null)?.removeAttribute('open'); }}/><a href="/auth/login">Sign in</a></nav>
          </details>
        </div>
      </header>
      <main id="main"><Outlet/></main>
      <footer className="site-footer">
        <div className="wrap footer-grid">
          <div>
            <Link className="brand" to="/"><Lark className="brand-mark"/><span>Alondra</span></Link>
            <p>Alondra turns a request into tasks and brings you the decisions.</p>
          </div>
          <nav aria-label="Footer">
            <Link to="/how-it-works">How it works</Link>
            <Link to="/install">Install</Link>
            <Link to="/docs">Documentation</Link>
            <Link to="/security">Security and privacy</Link>
            <Link to="/license">License</Link>
          </nav>
          <p className="footer-legal">Alondra is source-available under the Elastic License 2.0. The name and the logo are not part of the license.</p>
        </div>
      </footer>
    </div>
  );
}
