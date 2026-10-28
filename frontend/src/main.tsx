// Dev-only: install the mock Tauri IPC BEFORE anything imports @tauri-apps/api
// (this side-effect import must stay first). It self-guards to dev + plain
// browser and is dead-code-eliminated from production builds.
import "./devMock";

import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";
import { installLinkGuard } from "./safeContent";
import { CloudConsentOnLaunch } from "./components/CloudConsent";
import { ConfirmProvider } from "./components/confirm";

// Links anywhere in the app open in the system browser, never in this window.
installLinkGuard();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {/* Every confirm in the app is an in-app dialog (useConfirm). */}
    <ConfirmProvider>
      <App />
      <CloudConsentOnLaunch />
    </ConfirmProvider>
  </React.StrictMode>,
);
