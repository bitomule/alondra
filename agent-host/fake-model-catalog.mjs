import { createInterface } from 'node:readline';
import { appendFileSync } from 'node:fs';

const lines = createInterface({ input: process.stdin });
lines.on('line', (raw) => {
  const message = JSON.parse(raw);
  appendFileSync(process.env.FIXTURE_CAPTURE, JSON.stringify(message) + '\n');
  if (message.type === 'user') throw new Error('A model catalog probe must not send a user prompt');
  if (message.type !== 'control_request' || message.request.subtype !== 'initialize') throw new Error('Unexpected model probe request');
  process.stdout.write(JSON.stringify({ type: 'control_response', response: { subtype: 'success', request_id: message.request_id, response: {
    commands: [], output_style: 'default', account: {}, models: [
      { value: 'native-first', displayName: 'First native model', description: 'First does not mean default', supportsEffort: true, supportedEffortLevels: ['low', 'high'] },
      { value: 'native-second', displayName: 'Second native model', description: 'Another available model' },
    ],
  } } }) + '\n');
});
