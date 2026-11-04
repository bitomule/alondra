import { type RouteConfig, index, layout, route } from '@react-router/dev/routes';
export default [
  layout('routes/public-layout.tsx', [
    index('routes/landing.tsx'),
    route('how-it-works', 'routes/how-it-works.tsx'),
    route('install', 'routes/install.tsx'),
    route('docs', 'routes/docs.tsx'),
    route('security', 'routes/security.tsx'),
    route('privacy', 'routes/privacy.tsx'),
    route('license', 'routes/license.tsx'),
  ]),
  layout('routes/private-layout.tsx', [route('invite', 'routes/invite.tsx'), route('login-error', 'routes/login-error.tsx'), route('account-closed', 'routes/account-closed.tsx'), route('account', 'routes/account.tsx'), route('admin', 'routes/admin.tsx')]),
] satisfies RouteConfig;
