import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { ChatPage } from '@/pages/chat';
import { AssistantConnection } from './connection/assistant-connection';
import { ConnectionBootstrap } from './connection/connection-bootstrap';
import './styles/global.css';
const root = document.getElementById('root');
if (!root) throw new Error('Missing application root');
createRoot(root).render(
  <StrictMode>
    <ConnectionBootstrap>
      <AssistantConnection />
      <ChatPage />
    </ConnectionBootstrap>
  </StrictMode>,
);
