import { Link, Outlet, useLocation } from 'react-router';
import '../styles.css';
export default function PrivateLayout() {
 const privateRoute = /^\/(account|admin)(\/|$)/.test(useLocation().pathname);
 return <><header><Link className="wordmark" to="/">alondra<span aria-hidden="true">↗</span></Link><nav aria-label="Principal"><Link to="/privacy">Privacidad</Link><Link to="/install">Instalar</Link>{privateRoute ? <Link to="/account">Mi cuenta</Link> : <a href="/auth/login">Entrar ↗</a>}</nav></header><main id="main"><Outlet/></main><footer><Link className="wordmark" to="/">alondra</Link><span>Una alpha pequeña. Un espacio propio.</span><Link to="/privacy">Cómo cuidamos tus datos</Link></footer></>;
}
