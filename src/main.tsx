import React from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import './styles.css';
import { AreaSelector } from './video/AreaSelector';

createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {new URLSearchParams(window.location.search).has('screenSelector') ? <AreaSelector /> : <App />}
  </React.StrictMode>,
);