import { useState } from 'react';
import { cancel } from '@/entities/chat';
import { Button } from '@/shared/ui/button';
export function StopButton({ runId }: { runId: string }) {
  const [error, setError] = useState('');
  return (
    <>
      <Button
        className="stop"
        onClick={() => {
          void cancel(runId).catch(() => setError('Could not stop. Try again.'));
        }}
      >
        ■ <span>Stop</span>
      </Button>
      {error && <span role="alert">{error}</span>}
    </>
  );
}
