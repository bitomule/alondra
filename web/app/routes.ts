import { type RouteConfig, index, layout, route } from '@react-router/dev/routes';
export default [
  index('routes/landing.tsx'),
  route('proposal/:locale', 'routes/proposal.tsx'),
  layout('routes/public-layout.tsx', [route('privacy', 'routes/privacy.tsx'), route('install', 'routes/install.tsx')]),
  layout('routes/private-layout.tsx', [route('invite', 'routes/invite.tsx'), route('login-error', 'routes/login-error.tsx'), route('account-closed', 'routes/account-closed.tsx'), route('account', 'routes/account.tsx'), route('admin', 'routes/admin.tsx')]),
] satisfies RouteConfig;
