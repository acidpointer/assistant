import { post, apiVersion } from '@/shared/api';
import { parseRun } from './contracts';
export async function submit(chatId: string, client: string, key: string, text: string) {
  return parseRun(
    await post(apiVersion() === 2 ? '/runs' : `/chats/${encodeURIComponent(chatId)}/runs`, {
      client_id: client,
      client_request_id: key,
      input: { type: 'text', text },
    }),
  );
}
export async function lookup(client: string, key: string) {
  return parseRun(await post('/operations/lookup', { client_id: client, client_request_id: key }));
}
export async function cancel(runId: string) {
  return parseRun(await post(`/runs/${encodeURIComponent(runId)}/cancel`, {}));
}
