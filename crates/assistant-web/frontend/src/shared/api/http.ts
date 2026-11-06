/** Unknown JSON stays unknown until a domain decoder validates it. */
export class RequestError extends Error {
  constructor(
    readonly code: string,
    readonly status: number,
  ) {
    super(code);
  }
}
let version = 1;
export function configureApi(apiVersion: number) {
  version = apiVersion;
}
export function apiVersion() {
  return version;
}
export async function response(path: string, options: RequestInit = {}): Promise<Response> {
  const headers = new Headers(options.headers);
  headers.set('Content-Type', 'application/json');
  headers.set('X-Assistant-Client', 'webui');
  const result = await fetch(`/api/v${version}${path}`, {
    ...options,
    credentials: 'omit',
    headers,
  });
  if (!result.ok) {
    const failure: unknown = await result.json().catch(() => null);
    const code =
      failure &&
      typeof failure === 'object' &&
      'code' in failure &&
      typeof failure.code === 'string'
        ? failure.code
        : `http_${result.status}`;
    throw new RequestError(code, result.status);
  }
  return result;
}
export async function request(path: string, options: RequestInit = {}): Promise<unknown> {
  const result = await response(path, options);
  const body: unknown = await result.json();
  return body;
}
export function post(path: string, body: unknown): Promise<unknown> {
  return request(path, { method: 'POST', body: JSON.stringify(body) });
}
