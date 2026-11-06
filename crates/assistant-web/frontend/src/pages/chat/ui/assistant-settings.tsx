import { useEffect, useState } from 'react';
import * as v from 'valibot';
import { request, RequestError } from '@/shared/api';
import { Button } from '@/shared/ui/button';

const profileSchema = v.object({
  revision: v.string(),
  settings: v.object({
    instructions: v.string(),
    initiative_interval_seconds: v.nullable(v.number()),
  }),
  next_check_at: v.nullable(v.number()),
  last_check: v.nullable(v.string()),
});
type Profile = v.InferOutput<typeof profileSchema>;
/** Freeform behavior belongs to the assistant, independently of any chat tab. */
export function AssistantSettings({ close }: { close: () => void }) {
  const [profile, setProfile] = useState<Profile | null>(null);
  const [instructions, setInstructions] = useState('');
  const [initiative, setInitiative] = useState(false);
  const [minutes, setMinutes] = useState(30);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState('');
  const [error, setError] = useState('');
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    void request('/assistant', { signal: controller.signal })
      .then((value) => {
        if (controller.signal.aborted) return;
        const next = v.parse(profileSchema, value);
        setProfile(next);
        setInstructions(next.settings.instructions);
        setInitiative(next.settings.initiative_interval_seconds !== null);
        setMinutes((next.settings.initiative_interval_seconds ?? 1800) / 60);
      })
      .catch(() => {
        if (!controller.signal.aborted) setError('Could not load assistant settings.');
      });
    return () => controller.abort();
  }, [retry]);
  return (
    <aside className="memory-panel" aria-label="Assistant settings">
      <div className="memory-heading">
        <h2>Your assistant</h2>
        <Button onClick={close}>Close</Button>
      </div>
      <p>
        Describe the personality, role, style and preferences you want. These instructions stay with
        your assistant across conversations and restarts.
      </p>
      {profile && (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (busy || !instructions.trim()) return;
            setBusy(true);
            setError('');
            setStatus('');
            void request('/assistant', {
              method: 'PUT',
              body: JSON.stringify({
                revision: profile.revision,
                settings: {
                  instructions,
                  initiative_interval_seconds: initiative ? Math.round(minutes * 60) : null,
                },
              }),
            })
              .then((value) => {
                setProfile(v.parse(profileSchema, value));
                setStatus('Saved. Your assistant will use these instructions from its next turn.');
              })
              .catch((failure: unknown) =>
                setError(
                  failure instanceof RequestError && failure.status === 409
                    ? 'Settings changed elsewhere. Reload them before editing again.'
                    : 'Could not save settings.',
                ),
              )
              .finally(() => setBusy(false));
          }}
        >
          <label htmlFor="assistant-personality">Personality and behavior</label>
          <textarea
            id="assistant-personality"
            rows={9}
            maxLength={4096}
            required
            value={instructions}
            onChange={(event) => setInstructions(event.target.value)}
          />
          <label htmlFor="assistant-initiative">
            <input
              id="assistant-initiative"
              type="checkbox"
              checked={initiative}
              onChange={(event) => setInitiative(event.target.checked)}
            />{' '}
            Let my assistant start a conversation
          </label>
          <p>
            When enabled, your assistant occasionally considers a useful question, reminder or
            suggestion using its memory. It can choose to stay quiet.
          </p>
          {initiative && (
            <label htmlFor="initiative-minutes">
              Check at most every (minutes)
              <input
                id="initiative-minutes"
                type="number"
                min={1}
                max={1440}
                step={1}
                required
                value={minutes}
                onChange={(event) => setMinutes(Number(event.target.value))}
              />
            </label>
          )}
          <div className="memory-actions">
            <Button type="submit" disabled={busy}>
              {busy ? 'Saving…' : 'Save assistant'}
            </Button>
            <Button
              type="button"
              disabled={busy}
              onClick={() => {
                setError('');
                setRetry((value) => value + 1);
              }}
            >
              Reload settings
            </Button>
          </div>
          {profile.next_check_at && (
            <p>Next consideration: {new Date(profile.next_check_at).toLocaleString()}</p>
          )}
          {profile.last_check && (
            <p>Last consideration: {profile.last_check.split('_').join(' ')}</p>
          )}
        </form>
      )}
      {status && <output>{status}</output>}
      {error && <p role="alert">{error}</p>}
    </aside>
  );
}
