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

const GITHUB = 'https://github.com/bitomule/alondra';

function GitHubMark() {
  return <svg className="github-mark" viewBox="0 0 16 16" width="20" height="20" aria-hidden="true" focusable="false"><path fill="currentColor" d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8z"/></svg>;
}

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
            <Link className="button button-small" to="/#cloud">Join the waitlist</Link>
            <a className="github-link" href={GITHUB} aria-label="Alondra on GitHub" rel="noopener"><GitHubMark/></a>
          </div>
          <details className="menu">
            <summary>Menu</summary>
            <nav aria-label="Main, compact"><Links onClick={() => { (document.querySelector('.menu') as HTMLDetailsElement | null)?.removeAttribute('open'); }}/><a href={GITHUB} rel="noopener">GitHub</a></nav>
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
            <a href={GITHUB} rel="noopener">GitHub</a>
          </nav>
          <div className="footer-legal">
            <p>Alondra is source-available under the Elastic License 2.0. The name and the logo are not part of the license.</p>
            <p>Already have an Alondra Cloud account? <a className="text-link" href="/auth/login">Sign in</a></p>
          </div>
        </div>
      </footer>
    </div>
  );
}
