import { useEffect, useRef } from 'react';
export interface Action {
  id: string;
  title: string;
  hint: string;
  execute: () => void;
}
export function CommandPalette({
  actions,
  close,
}: {
  actions: readonly Action[];
  close: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const previous = document.activeElement;
    dialog.current?.showModal();
    return () => {
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);
  return (
    <dialog ref={dialog} className="command-palette" onCancel={close} aria-label="Quick actions">
      <div className="palette-title">
        Quick actions{' '}
        <button type="button" onClick={close} aria-label="Close actions">
          Esc
        </button>
      </div>
      {actions.map((action) => (
        <button
          type="button"
          key={action.id}
          onClick={() => {
            close();
            action.execute();
          }}
        >
          <span>{action.title}</span>
          <kbd>{action.hint}</kbd>
        </button>
      ))}
      <p>Use Tab and Enter to choose an action.</p>
    </dialog>
  );
}
