import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import './style.css';

// StrictMode is deliberate: it double-mounts effects in dev, which is
// exactly the condition that shakes out register/unregister lifecycle bugs.
createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
