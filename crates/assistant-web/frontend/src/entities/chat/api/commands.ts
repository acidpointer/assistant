import { post } from '@/shared/api';
import { parseRun } from './contracts';
export async function submit(chatId: string, client: string, key: string, text: string) {
  return parseRun(
    await post(`/chats/${encodeURIComponent(chatId)}/runs`, {
      client_id: client,
      client_request_id: key,
      input: { type: 'text', text },
    }),
  );
}
export async function cancel(runId: string) {
  return parseRun(await post(`/runs/${encodeURIComponent(runId)}/cancel`, {}));
}
