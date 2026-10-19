import { createHmac, timingSafeEqual } from 'node:crypto';
import { executionSegments, type Frame } from './frame.js';

const OWNER_PROOF_DOMAIN = 'alondra-host-owner-v1';
const HOST_PROOF_DOMAIN = 'alondra-host-accept-v1';
export type Attach = Extract<Frame['body'], { type: 'attach' }>;
export function ownerProof(secret: Buffer, attach: Omit<Attach, 'proof' | 'type'>): string { return claimProof(secret, attach, OWNER_PROOF_DOMAIN); }
export function hostProof(secret: Buffer, attach: Attach): string { return claimProof(secret, attach, HOST_PROOF_DOMAIN, attach.proof); }
function claimProof(secret: Buffer, attach: Omit<Attach, 'proof' | 'type'>, domain: string, requestProof?: string): string {
  if (secret.length !== 32) throw new Error('Control secret length');
  const { fence, execution } = attach;
  const segments = [domain, attach.nonce, attach.host_instance_id, String(attach.owner_generation),
    fence.profile_id, fence.account_id, fence.data_epoch, fence.host_id,
    ...executionSegments(execution),
    attach.host_release, attach.host_sha256, attach.executable_sha256, attach.sdk_version];
  if (requestProof !== undefined) segments.push(requestProof);
  const hmac = createHmac('sha256', secret);
  for (const segment of segments) {
    const bytes = Buffer.from(segment); const length = Buffer.alloc(4); length.writeUInt32BE(bytes.length);
    hmac.update(length); hmac.update(bytes);
  }
  return hmac.digest('hex');
}
export function proofMatches(secret: Buffer, attach: Attach): boolean {
  return timingSafeEqual(Buffer.from(ownerProof(secret, attach), 'hex'), Buffer.from(attach.proof, 'hex'));
}
