import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { ChatPage } from '@/pages/chat';
import { AssistantConnection } from './connection/assistant-connection';
import './styles/global.css';
const root = document.getElementById('root');
if (!root) throw new Error('Missing application root');
createRoot(root).render(
  <StrictMode>
    <AssistantConnection />
    <ChatPage />
  </StrictMode>,
);
