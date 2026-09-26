import { Links, Meta, Outlet, Scripts, ScrollRestoration, Link, useLocation } from 'react-router';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useState } from 'react';
import './styles.css';
export function Layout({ children }: { children: React.ReactNode }) {
 return <html lang="es"><head><meta charSet="utf-8"/><meta name="viewport" content="width=device-width, initial-scale=1"/><meta name="referrer" content="no-referrer"/><Meta/><Links/></head><body><a className="skip-link" href="#main">Saltar al contenido</a>{children}<ScrollRestoration/><Scripts/></body></html>;
}
export default function App() {
 const privateRoute = /^\/(account|admin)(\/|$)/.test(useLocation().pathname);
 const [client] = useState(() => new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: 15000 }, mutations: { retry: false } } }));
 return <QueryClientProvider client={client}><header><Link className="wordmark" to="/">alondra<span aria-hidden="true">↗</span></Link><nav aria-label="Principal"><Link to="/privacy">Privacidad</Link><Link to="/install">Instalar</Link>{privateRoute ? <Link to="/account">Mi cuenta</Link> : <a href="/auth/login">Entrar ↗</a>}</nav></header><main id="main"><Outlet/></main><footer><Link className="wordmark" to="/">alondra</Link><span>Una alpha pequeña. Un espacio propio.</span><Link to="/privacy">Cómo cuidamos tus datos</Link></footer></QueryClientProvider>;
}
