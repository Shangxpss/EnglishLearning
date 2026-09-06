import ReactDOM from "react-dom/client";
import { StrictMode } from 'react';
import { RouterProvider } from 'react-router/dom';
import { createBrowserRouter } from 'react-router';
import { routes } from './router/routes';
import { RootProvider } from './providers';
import './index.css';

const router = createBrowserRouter(routes);
const root = document.getElementById("root");
ReactDOM.createRoot(root!).render(
  <StrictMode>
    <RootProvider>
      <RouterProvider router={router} />
    </RootProvider>
  </StrictMode>,
);