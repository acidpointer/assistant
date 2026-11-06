import { useCallback, useEffect, useRef, useState } from 'react';
import { cancel, useChat, useMessageIds } from '@/entities/chat';
import { OverallPerformance } from './performance';
import { Button } from '@/shared/ui/button';
import { Composer } from './composer';
import { MessageRow } from './message-row';
import { CommandPalette } from './command-palette';
import type { Action } from './command-palette';
import { apiVersion } from '@/shared/api';
import { AssistantSettings } from './assistant-settings';
import { AssistantNotices } from './assistant-notices';
import { MemoryPanel } from './memory-panel';

/** The page owns chat-only actions. Shared entity state and transport stay separate.
 * Resize-driven scroll anchoring follows only when the reader stays near the end. */
export function ChatPage() {
  const chat = useChat();
  const ids = useMessageIds();
  const [palette, setPalette] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [memoryOpen, setMemoryOpen] = useState(false);
  const [atEnd, setAtEnd] = useState(true);
  const scroller = useRef<HTMLElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const [actionError, setActionError] = useState('');
  const closePalette = useCallback(() => setPalette(false), []);
  const jump = useCallback(() => {
    const node = scroller.current;
    pinned.current = true;
    setAtEnd(true);
    if (node) node.scrollTop = node.scrollHeight;
  }, []);
  const stop = useCallback(() => {
    if (chat.activeRunId)
      void cancel(chat.activeRunId).catch(() => setActionError('Could not stop. Try again.'));
  }, [chat.activeRunId]);
  const actions: Action[] = [
    {
      id: 'focus',
      title: 'Focus message',
      hint: 'Write',
      execute: () => document.getElementById('prompt')?.focus(),
    },
    { id: 'latest', title: 'Jump to latest', hint: 'End', execute: jump },
    ...(chat.activeRunId
      ? [{ id: 'stop', title: 'Stop generation', hint: 'Esc', execute: stop }]
      : []),
  ];
  useEffect(() => {
    const listener = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault();
        setPalette((open) => !open);
      } else if (event.key === 'Escape' && !palette && chat.activeRunId) {
        event.preventDefault();
        stop();
      }
    };
    document.addEventListener('keydown', listener);
    return () => document.removeEventListener('keydown', listener);
  }, [palette, chat.activeRunId, stop]);
  useEffect(() => {
    const node = content.current;
    if (!node) return;
    const observer = new ResizeObserver(() => {
      if (pinned.current) jump();
    });
    observer.observe(node);
    const viewport = window.visualViewport;
    const resize = () => {
      document.documentElement.style.setProperty(
        '--viewport-height',
        `${viewport?.height ?? window.innerHeight}px`,
      );
      if (pinned.current) jump();
    };
    resize();
    viewport?.addEventListener('resize', resize);
    window.addEventListener('resize', resize);
    return () => {
      observer.disconnect();
      viewport?.removeEventListener('resize', resize);
      window.removeEventListener('resize', resize);
    };
  }, [jump]);
  return (
    <div className={`shell${apiVersion() === 2 ? ' shell-memory' : ''}`}>
      <header className="topbar">
        <div className="brand">
          <span className="brand-symbol">✦</span>
          <span>
            assistant<span className="brand-period">.</span>
          </span>
          <span className="local-badge">LOCAL</span>
        </div>
        <div className="topbar-actions">
          {apiVersion() === 2 && (
            <>
              <Button onClick={() => setSettingsOpen((open) => !open)}>Assistant</Button>
              <Button onClick={() => setMemoryOpen((open) => !open)}>Memory</Button>
            </>
          )}
          <OverallPerformance />
          <Button
            className="command-trigger"
            onClick={() => setPalette(true)}
            aria-label="Quick actions"
          >
            ⌘ <kbd>K</kbd>
          </Button>
        </div>
      </header>
      <main className="workspace">
        {/* Keyboard focus enables scrolling this independently scrollable region. */}
        <section
          className="transcript"
          ref={scroller}
          // oxlint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- Independently scrollable regions must support keyboard focus.
          tabIndex={0}
          aria-label="Conversation"
          onScroll={() => {
            const node = scroller.current;
            if (node) {
              pinned.current = node.scrollHeight - node.scrollTop - node.clientHeight < 80;
              setAtEnd(pinned.current);
            }
          }}
        >
          <div className="conversation" ref={content}>
            {!ids.length && (
              <section className="welcome">
                <div className="welcome-orb">✦</div>
                <p className="eyebrow">YOUR SPACE TO THINK</p>
                <h1>
                  A little clarity.
                  <br />
                  <span>A lot of possibility.</span>
                </h1>
                <p>
                  Ask a question, untangle an idea, or start something new.
                  <br className="desktop-hint" /> Your assistant runs right here, on your device.
                </p>
                <div className="welcome-chips">
                  <span>Private by design</span>
                  <span>Always within reach</span>
                </div>
              </section>
            )}
            {chat.truncated && (
              <p className="retention-note">
                {apiVersion() === 2
                  ? 'Older exchanges are saved. Open Memory to browse saved history.'
                  : 'Older exchanges have left the temporary transcript.'}
              </p>
            )}
            {ids.map((id) => (
              <MessageRow key={id} id={id} />
            ))}
            {apiVersion() === 2 && <AssistantNotices />}
            {chat.activeRunId && (
              <div className="activity">
                <span className="activity-pulse" />
                {chat.activity || 'Thinking'}
              </div>
            )}
          </div>
        </section>
        {!atEnd && (
          <Button className="jump-latest" onClick={jump}>
            ↓ Latest
          </Button>
        )}
        <div className="composer-region">
          <Composer />
          <output className="statusline" aria-live="polite">
            <span className={`connection-dot ${chat.connection === 'online' ? 'connected' : ''}`} />
            {chat.connection === 'online'
              ? chat.activeRunId
                ? 'Working on your device'
                : 'Ready on your device'
              : 'Reconnecting to assistant…'}
            <span className="status-right">
              {apiVersion() === 2
                ? 'Saved locally · semantic memory'
                : 'Temporary conversation · stays in memory'}
            </span>
          </output>
          {actionError && <p role="alert">{actionError}</p>}
        </div>
      </main>
      {settingsOpen && <AssistantSettings close={() => setSettingsOpen(false)} />}
      {memoryOpen && <MemoryPanel close={() => setMemoryOpen(false)} />}
      {palette && <CommandPalette actions={actions} close={closePalette} />}
    </div>
  );
}
