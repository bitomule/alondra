import type { Config } from '@react-router/dev/config';
// Private routes contain only an unauthenticated loading shell at build time.
export default { ssr: false, prerender: ['/proposal/en', '/proposal/es', '/', '/privacy', '/install', '/account', '/admin', '/invite', '/login-error', '/account-closed'] } satisfies Config;
