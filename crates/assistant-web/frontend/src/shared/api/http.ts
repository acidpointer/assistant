/** Unknown JSON stays unknown until a domain decoder validates it. */
export class RequestError extends Error {
  constructor(
    readonly code: string,
    readonly status: number,
  ) {
    super(code);
  }
}
export async function request(path: string, options: RequestInit = {}): Promise<unknown> {
  const headers = new Headers(options.headers);
  headers.set('Content-Type', 'application/json');
  headers.set('X-Assistant-Client', 'webui');
  const response = await fetch(`/api/v1${path}`, {
    ...options,
    credentials: 'same-origin',
    headers,
  });
  if (!response.ok) throw new RequestError(`http_${response.status}`, response.status);
  const body: unknown = await response.json();
  return body;
}
export function post(path: string, body: unknown): Promise<unknown> {
  return request(path, { method: 'POST', body: JSON.stringify(body) });
}
